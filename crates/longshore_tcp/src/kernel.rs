// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The kernel's TCP through the `Io` trait: io_uring on Linux, the portable
//! backend, and the simulator.
//!
//! Slots map to socket slots of the shared `Io` and operations to tokens in
//! the `Net`'s range: token = base + index * 8 + kind, where the index is a
//! connection slot, or the connection count plus a listener slot.

use crate::config::Config;
use crate::transport::{Happened, Refused};
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
