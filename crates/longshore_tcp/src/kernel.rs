// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The kernel's TCP through the `Io` trait: io_uring on Linux, the portable
//! backend, and the simulator.
//!
//! Slots map to socket slots of the shared `Io` and operations to tokens in
//! the `Net`'s range: token = base + index * 8 + kind, where the index is a
//! connection slot, or the connection count plus a listener slot. Behind
//! those, a zero-copy send whose memory outlives its report has a token
//! of its own, by slot and by the number `Net` gave it, so that the end
//! of each is told apart whatever order they come in.

use crate::config::Config;
use crate::transport::{Happened, Refused, Span};
use core::marker::PhantomData;
use longshore_common::io::{Completion, Io, Op, Shut, Sock, Token};

#[cfg(not(feature = "tls-core"))]
#[path = "kernel/plain.rs"]
mod secure;
#[cfg(feature = "tls-core")]
mod secure;

use secure::Secure;

mod cover;
mod transport;
pub use cover::PhaseCoverage;
#[cfg(feature = "tls-core")]
pub use secure::Tls;

const RECV: u64 = 0;
const SEND: u64 = 1;
const CONNECT: u64 = 2;
const CLOSE: u64 = 3;
const SHUTDOWN: u64 = 4;
const CANCEL: u64 = 5;
const ACCEPT: u64 = 6;

/// Kernel TCP over an `Io` the caller owns.
pub struct KernelTransport<IO: Io> {
    token_base: u64,
    conns: u32,
    listeners: u32,
    /// Socket slot of listener 0; connection 0 follows the listeners.
    sock_base: u32,
    /// Per connection slot: an armed receive is in flight.
    armed: Box<[bool]>,
    /// Per connection slot: the listener whose single accept will fill it.
    accepting: Box<[u32]>,
    /// Per connection slot, then per listener: the kinds of operation
    /// the transport has in flight, a bit each. Kept to assert that `Net`
    /// never starts a second of a kind.
    flight: Box<[u8]>,
    /// The first token of the zero-copy sends reported twice, and how
    /// many a connection has.
    lent_base: u64,
    lends: u32,
    /// Per such send: its bytes were taken and reported; its end is due.
    taken: Box<[bool]>,
    /// TLS, where it is compiled in and set up.
    secure: Secure,
    io: PhantomData<fn(&mut IO)>,
}

impl<IO: Io> KernelTransport<IO> {
    /// A transport for the share of the backend that `config` describes.
    /// Allocates its two small tables once, here.
    pub fn new(config: &Config) -> KernelTransport<IO> {
        let conns = config.conns();
        KernelTransport {
            token_base: config.token_base(),
            conns,
            listeners: config.sizes.listeners,
            sock_base: config.sock_base,
            armed: vec![false; conns as usize].into_boxed_slice(),
            accepting: vec![0; conns as usize].into_boxed_slice(),
            flight: vec![0; (conns + config.sizes.listeners) as usize].into_boxed_slice(),
            lent_base: config.token_base() + config.lent_tokens(),
            lends: config.sizes.zero_copy_sends,
            taken: vec![false; (conns * config.sizes.zero_copy_sends) as usize].into_boxed_slice(),
            secure: Secure::new(conns),
            io: PhantomData,
        }
    }

    /// The transport with TLS: `tls` says which sides it can take. The
    /// `Net`'s `Sizes` must have `with_tls`.
    #[cfg(feature = "tls-core")]
    #[must_use]
    pub fn with_tls(mut self, tls: Tls) -> KernelTransport<IO> {
        self.secure.set(tls);
        self
    }

    fn conn_sock(&self, slot: u32) -> Sock {
        Sock::new(self.sock_base + self.listeners + slot)
    }

    fn listener_sock(&self, listener: u32) -> Sock {
        Sock::new(self.sock_base + listener)
    }

    fn conn_token(&self, slot: u32, kind: u64) -> Token {
        Token::new(self.token_base + u64::from(slot) * 8 + kind)
    }

    /// Submit `op` as the operation of `kind` for `index`: a connection
    /// slot, or the number of connections plus a listener.
    fn start(&mut self, d: &mut IO, index: u32, kind: u64, op: Op) -> Result<(), Refused> {
        let token = Token::new(self.token_base + u64::from(index) * 8 + kind);
        let bit = 1u8 << kind;
        // A cancel names another operation and may be asked for again.
        debug_assert!(
            kind == CANCEL || self.flight[index as usize] & bit == 0,
            "a second operation of kind {kind} for {index} while one is in flight"
        );
        d.submit(token, op).map_err(|_| Refused::Busy)?;
        self.flight[index as usize] |= bit;
        Ok(())
    }

    /// Send `from` on `slot` from where it lies, as send `id` of the
    /// slot's sends whose memory outlives their report.
    fn send_lent(&mut self, d: &mut IO, slot: u32, id: u32, from: Span) -> Result<(), Refused> {
        if id >= self.lends {
            return Err(Refused::Unsupported);
        }
        let at = slot * self.lends + id;
        let bit = 1u8 << SEND;
        debug_assert!(
            self.flight[slot as usize] & bit == 0 && !self.taken[at as usize],
            "send {id} of {slot} while one is unreported, or its own memory is still read"
        );
        let op = Op::SendZc {
            sock: self.conn_sock(slot),
            buf: from.buf(),
            off: from.off(),
            len: from.len(),
        };
        let token = Token::new(self.lent_base + u64::from(at));
        d.submit(token, op).map_err(|_| Refused::Busy)?;
        self.flight[slot as usize] |= bit;
        Ok(())
    }

    /// A completion of the `at`-th zero-copy send that reports twice:
    /// its bytes taken and its memory still read, or its end. An end that
    /// no such report went before is the whole send.
    fn lent_happened(&mut self, at: u64, c: Completion) -> Option<Happened> {
        let at = usize::try_from(at).ok().filter(|i| *i < self.taken.len())?;
        let (slot, id) = (at as u32 / self.lends, at as u32 % self.lends);
        if !c.more() && core::mem::take(&mut self.taken[at]) {
            return Some(Happened::Released { slot, id });
        }
        // The send is reported: the slot's next may start.
        self.flight[slot as usize] &= !(1u8 << SEND);
        self.taken[at] = c.more();
        let result = c.result();
        Some(if c.more() {
            Happened::Taken { slot, result }
        } else {
            Happened::Sent { slot, result }
        })
    }

    /// A completion on a connection slot.
    fn conn_happened(&mut self, d: &mut IO, slot: u32, kind: u64, c: Completion) -> Happened {
        let result = c.result();
        match kind {
            RECV => {
                let was_armed = core::mem::replace(&mut self.armed[slot as usize], c.more());
                match c.buf() {
                    Some(buf) => Happened::RingData {
                        slot,
                        buf,
                        len: result.unsigned_abs(),
                        armed: c.more(),
                    },
                    None if was_armed => Happened::RecvEnded { slot, result },
                    None if self.secure.is_on(slot) => self.on_stream(d, slot, false, result),
                    None => Happened::Received { slot, result },
                }
            }
            SEND if self.secure.is_on(slot) => self.on_stream(d, slot, true, result),
            SEND => Happened::Sent { slot, result },
            CONNECT => Happened::Connected { slot, result },
            // An armed receive's own last completion clears its flag; it may
            // come after the close's.
            CLOSE => {
                self.secure.closed(slot);
                Happened::Closed { slot }
            }
            SHUTDOWN => Happened::ShutDown { slot, result },
            ACCEPT => {
                let listener = self.accepting[slot as usize];
                if result >= 0 {
                    Happened::Accepted {
                        listener,
                        slot,
                        armed: false,
                    }
                } else {
                    Happened::AcceptFailed {
                        listener,
                        slot: Some(slot),
                        result,
                    }
                }
            }
            // A cancel's own completion says nothing new: the operation it
            // cancelled reports its end itself.
            _ => Happened::Nothing,
        }
    }

    /// A send or receive completion of a slot whose stream is protected.
    fn on_stream(&mut self, d: &mut IO, slot: u32, sent: bool, result: i32) -> Happened {
        let shutdown = (
            self.conn_token(slot, SHUTDOWN),
            Op::Shutdown {
                sock: self.conn_sock(slot),
                how: Shut::Write,
            },
        );
        self.secure.on_io(d, slot, sent, result, shutdown)
    }

    /// A completion on a listener slot.
    fn listener_happened(&self, listener: u32, kind: u64, c: Completion) -> Happened {
        match kind {
            ACCEPT => {
                // An armed accept names the socket slot it filled.
                let first = self.sock_base + self.listeners;
                let slot = u32::try_from(c.result())
                    .ok()
                    .and_then(|sock| sock.checked_sub(first))
                    .filter(|slot| *slot < self.conns);
                match slot {
                    Some(slot) => Happened::Accepted {
                        listener,
                        slot,
                        armed: c.more(),
                    },
                    None => Happened::AcceptFailed {
                        listener,
                        slot: None,
                        result: c.result().min(-1),
                    },
                }
            }
            CLOSE => Happened::ListenerClosed { listener },
            _ => Happened::Nothing,
        }
    }
}
