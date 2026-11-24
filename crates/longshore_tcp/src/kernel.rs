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

use crate::addr::Addr;
use crate::config::Config;
use crate::transport::{Happened, Listening, Opened, Refused, Role, Span, Transport};
use core::marker::PhantomData;
use longshore_common::io::{BufId, Completion, Io, ListenOpts, Op, Shut, Sock, Token};
use std::net::SocketAddr;

#[cfg(not(feature = "tls-core"))]
#[path = "kernel/plain.rs"]
mod secure;
#[cfg(feature = "tls-core")]
mod secure;

use secure::Secure;
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

    fn listener_token(&self, listener: u32, kind: u64) -> Token {
        Token::new(self.token_base + u64::from(self.conns + listener) * 8 + kind)
    }

    fn submit(d: &mut IO, token: Token, op: Op) -> Result<(), Refused> {
        d.submit(token, op).map_err(|_| Refused::Busy)
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
                    None if was_armed => Happened::RecvEnded {
                        slot,
                        result: self.secure.end_of_stream(slot, result),
                    },
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

impl<IO: Io> Transport for KernelTransport<IO> {
    type Driver = IO;
    type Completion = Completion;

    fn attach(&mut self, config: &Config) {
        let (socks, tokens) = (self.sock_base + self.listeners, self.token_base);
        self.secure.attach(config, |slot, kind| {
            let token = Token::new(tokens + u64::from(slot) * 8 + kind);
            (Sock::new(socks + slot), token)
        });
    }

    fn now_ns(&self, d: &IO) -> u64 {
        d.now_ns()
    }

    fn bytes<'d>(&self, d: &'d mut IO, buf: BufId) -> &'d mut [u8] {
        d.buf(buf)
    }

    fn bytes_pair<'d>(&self, d: &'d mut IO, a: BufId, b: BufId) -> (&'d mut [u8], &'d mut [u8]) {
        d.buf_pair(a, b)
    }

    fn listen(
        &mut self,
        d: &mut IO,
        listener: u32,
        addr: &Addr,
        how: Listening,
    ) -> std::io::Result<Option<SocketAddr>> {
        let sock = self.listener_sock(listener);
        match addr {
            Addr::Ip(ip) => {
                let opts = ListenOpts::DEFAULT
                    .with_reuse_port(how.reuse_port())
                    .with_backlog(how.backlog().max(1));
                d.listen_with(sock, *ip, opts).map(Some)
            }
            // A path belongs to one listener: there is no port to share.
            Addr::Unix(_) if how.reuse_port() => {
                Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
            }
            Addr::Unix(path) => d.listen_unix(sock, path.as_str()).map(|()| None),
        }
    }

    fn accept_armed(&mut self, d: &mut IO, listener: u32) -> Result<(), Refused> {
        let op = Op::ArmedAccept {
            listener: self.listener_sock(listener),
        };
        Self::submit(d, self.listener_token(listener, ACCEPT), op)
    }

    fn accept_into(&mut self, d: &mut IO, listener: u32, slot: u32) -> Result<(), Refused> {
        let op = Op::Accept {
            listener: self.listener_sock(listener),
            sock: self.conn_sock(slot),
        };
        Self::submit(d, self.conn_token(slot, ACCEPT), op)?;
        self.accepting[slot as usize] = listener;
        Ok(())
    }

    fn connect(
        &mut self,
        d: &mut IO,
        slot: u32,
        addr: &Addr,
        scratch: Span,
    ) -> Result<(), Refused> {
        let sock = self.conn_sock(slot);
        let op = match addr {
            Addr::Ip(ip) => Op::Connect { sock, addr: *ip },
            Addr::Unix(path) => {
                // The backend reads the path from the pool.
                let p = path.with_nul();
                if p.len() > scratch.len() as usize {
                    return Err(Refused::Unsupported);
                }
                let at = scratch.off() as usize;
                d.buf(scratch.buf())[at..at + p.len()].copy_from_slice(p);
                Op::ConnectUnix {
                    sock,
                    path_buf: scratch.buf(),
                    path_off: scratch.off(),
                    path_len: p.len() as u32,
                }
            }
        };
        Self::submit(d, self.conn_token(slot, CONNECT), op)
    }

    fn can_secure(&self, client: bool) -> bool {
        self.secure.can(client)
    }

    fn secure(&mut self, d: &mut IO, slot: u32, role: Role<'_>) -> Result<(), Refused> {
        self.secure.start(d, slot, role)
    }

    fn open(&mut self, d: &mut IO, slot: u32, from: Span, into: Span) -> Option<Opened> {
        self.secure.open(d, slot, from, into)
    }

    fn recv_armed(&mut self, d: &mut IO, slot: u32) -> Result<(), Refused> {
        let sock = self.conn_sock(slot);
        Self::submit(d, self.conn_token(slot, RECV), Op::ArmedRecv { sock })?;
        self.armed[slot as usize] = true;
        Ok(())
    }

    fn recv_into(&mut self, d: &mut IO, slot: u32, into: Span) -> Result<Option<i32>, Refused> {
        if self.secure.is_on(slot) {
            return Ok(self.secure.recv(d, slot, into));
        }
        let op = Op::Recv {
            sock: self.conn_sock(slot),
            buf: into.buf(),
            off: into.off(),
            len: into.len(),
        };
        Self::submit(d, self.conn_token(slot, RECV), op).map(|()| None)
    }

    fn cancel_recv(&mut self, d: &mut IO, slot: u32) -> Result<(), Refused> {
        let target = self.conn_token(slot, RECV);
        Self::submit(d, self.conn_token(slot, CANCEL), Op::Cancel { target })
    }

    fn give_back(&mut self, d: &mut IO, buf: BufId) {
        d.give_back(buf);
    }

    fn send(&mut self, d: &mut IO, slot: u32, from: Span, zero_copy: bool) -> Result<(), Refused> {
        if self.secure.is_on(slot) {
            // Sealed into the connection's own buffer and sent from there.
            self.secure.send(d, slot, from);
            return Ok(());
        }
        let op = Op::Send {
            sock: self.conn_sock(slot),
            buf: from.buf(),
            off: from.off(),
            len: from.len(),
            zero_copy,
        };
        Self::submit(d, self.conn_token(slot, SEND), op)
    }

    fn shutdown_write(&mut self, d: &mut IO, slot: u32) -> Result<(), Refused> {
        if self.secure.is_on(slot) {
            // A close alert first; the shutdown follows it.
            self.secure.say_goodbye(d, slot);
            return Ok(());
        }
        let op = Op::Shutdown {
            sock: self.conn_sock(slot),
            how: Shut::Write,
        };
        Self::submit(d, self.conn_token(slot, SHUTDOWN), op)
    }

    fn close(&mut self, d: &mut IO, slot: u32) -> Result<(), Refused> {
        let sock = self.conn_sock(slot);
        Self::submit(d, self.conn_token(slot, CLOSE), Op::Close { sock })?;
        self.secure.closing(slot);
        Ok(())
    }

    fn close_listener(&mut self, d: &mut IO, listener: u32) -> Result<(), Refused> {
        let sock = self.listener_sock(listener);
        Self::submit(d, self.listener_token(listener, CLOSE), Op::Close { sock })
    }

    fn peer_addr(&self, d: &IO, slot: u32) -> Option<SocketAddr> {
        d.peer_addr(self.conn_sock(slot))
    }

    fn translate(&mut self, d: &mut IO, completion: Completion) -> Option<Happened> {
        let rel = completion.token().get().checked_sub(self.token_base)?;
        let (index, kind) = (u32::try_from(rel / 8).ok()?, rel % 8);
        if index < self.conns {
            Some(self.conn_happened(d, index, kind, completion))
        } else if index - self.conns < self.listeners {
            Some(self.listener_happened(index - self.conns, kind, completion))
        } else {
            None
        }
    }
}
