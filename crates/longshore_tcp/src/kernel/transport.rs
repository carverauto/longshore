// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The kernel transport as a [`Transport`]: each operation as one
//! submission to the backend, or handed to the slot's stream when the
//! slot is protected.

use super::{KernelTransport, ACCEPT, CANCEL, CLOSE, CONNECT, RECV, SEND, SHUTDOWN};
use crate::addr::Addr;
use crate::config::Config;
use crate::transport::{Happened, Lend, Listening, Opened, Refused, Role, Span, Transport};
use longshore_common::io::{BufId, Completion, Io, ListenOpts, Op, Shut, Sock, Token};
use std::net::SocketAddr;

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
        self.start(d, self.conns + listener, ACCEPT, op)
    }

    fn accept_into(&mut self, d: &mut IO, listener: u32, slot: u32) -> Result<(), Refused> {
        let op = Op::Accept {
            listener: self.listener_sock(listener),
            sock: self.conn_sock(slot),
        };
        self.start(d, slot, ACCEPT, op)?;
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
        self.start(d, slot, CONNECT, op)
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
        self.start(d, slot, RECV, Op::ArmedRecv { sock })?;
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
        self.start(d, slot, RECV, op).map(|()| None)
    }

    fn recv_kept(&mut self, d: &mut IO, slot: u32) -> Result<bool, Refused> {
        if !self.secure.is_on(slot) {
            return Err(Refused::Unsupported);
        }
        Ok(self.secure.recv_kept(d, slot))
    }

    fn recv_zc(&mut self, d: &mut IO, slot: u32, credit: u32) -> Result<(), Refused> {
        let sock = self.conn_sock(slot);
        self.start(d, slot, RECV, Op::ArmedRecvZc { sock, credit })?;
        self.pieces[slot as usize] = true;
        Ok(())
    }

    fn give_back_piece(&mut self, d: &mut IO, page: BufId, off: u32, len: u32) {
        d.give_back_piece(page, off, len);
    }

    fn ring_was_full(&self, d: &IO) -> bool {
        d.ring_was_full()
    }

    fn cancel_recv(&mut self, d: &mut IO, slot: u32) -> Result<(), Refused> {
        let target = self.conn_token(slot, RECV);
        self.start(d, slot, CANCEL, Op::Cancel { target })
    }

    fn give_back(&mut self, d: &mut IO, buf: BufId) {
        d.give_back(buf);
    }

    fn send(&mut self, d: &mut IO, slot: u32, from: Span, lend: Lend) -> Result<(), Refused> {
        if self.secure.is_on(slot) {
            // Sealed into the connection's own buffer and sent from there.
            self.secure.send(d, slot, from);
            return Ok(());
        }
        let zero_copy = match lend {
            Lend::UntilReleased(id) => return self.send_lent(d, slot, id, from),
            Lend::UntilSent => true,
            Lend::No => false,
        };
        let op = Op::Send {
            sock: self.conn_sock(slot),
            buf: from.buf(),
            off: from.off(),
            len: from.len(),
            zero_copy,
        };
        self.start(d, slot, SEND, op)
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
        self.start(d, slot, SHUTDOWN, op)
    }

    fn close(&mut self, d: &mut IO, slot: u32) -> Result<(), Refused> {
        let sock = self.conn_sock(slot);
        self.start(d, slot, CLOSE, Op::Close { sock })?;
        self.secure.closing(slot);
        Ok(())
    }

    fn close_listener(&mut self, d: &mut IO, listener: u32) -> Result<(), Refused> {
        let sock = self.listener_sock(listener);
        self.start(d, self.conns + listener, CLOSE, Op::Close { sock })
    }

    fn peer_addr(&self, d: &IO, slot: u32) -> Option<SocketAddr> {
        d.peer_addr(self.conn_sock(slot))
    }

    fn translate(&mut self, d: &mut IO, completion: Completion) -> Option<Happened> {
        let token = completion.token().get();
        if let Some(at) = token.checked_sub(self.lent_base) {
            return self.lent_happened(at, completion);
        }
        let rel = token.checked_sub(self.token_base)?;
        let (index, kind) = (u32::try_from(rel / 8).ok()?, rel % 8);
        if !completion.more() {
            if let Some(flight) = self.flight.get_mut(index as usize) {
                *flight &= !(1u8 << kind);
            }
        }
        if index < self.conns {
            Some(self.conn_happened(d, index, kind, completion))
        } else if index - self.conns < self.listeners {
            Some(self.listener_happened(index - self.conns, kind, completion))
        } else {
            None
        }
    }
}
