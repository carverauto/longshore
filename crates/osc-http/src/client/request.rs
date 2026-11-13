// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Starting a request: choosing a connection, writing the head, sending.

use super::*;

impl Client {
    /// Begin a request. Its outcome arrives later as [`Handled::Done`] with
    /// `token`. Nothing is retained from `req` except its buffers.
    pub fn start<IO: Io>(&mut self, io: &mut IO, token: u64, req: &Request) -> Result<(), Error> {
        let now = io.now_ns();
        let fresh = |c: &Conn| {
            now.saturating_sub(c.opened_at) < self.cfg.max_age_ns && c.uses < self.cfg.max_requests
        };
        // Prefer a live connection to this host. Failing that, replace an
        // expired connection to this host before opening another, so an aged
        // connection never outlives its limit just because a slot was free.
        // Then an unused slot, then a connection idle on another host.
        let pick = self
            .conns
            .iter()
            .position(|c| c.state == State::Idle && c.host_id == req.host_id && fresh(c))
            .or_else(|| {
                self.conns
                    .iter()
                    .position(|c| c.state == State::Idle && c.host_id == req.host_id)
            })
            .or_else(|| self.conns.iter().position(|c| c.state == State::Closed))
            .or_else(|| self.conns.iter().position(|c| c.state == State::Idle))
            .ok_or(Error::Busy)?;

        if req.host.len() > 253 {
            return Err(Error::BadRequest);
        }
        // The head buffer is about to be overwritten, so the previous
        // response's headers are gone whether or not this request is accepted.
        self.conns[pick].fields = None;
        let head_len = {
            let buf = &mut io.buf(self.head_buf(pick))[..self.cfg.head_max as usize];
            let mut w = HeadWriter::request(buf, req.method, req.target);
            w.header("Host", req.host.as_bytes());
            if req.body.is_some() || matches!(req.method, "PUT" | "POST") {
                w.header_u64("Content-Length", req.body.map_or(0, |b| b.1 as u64));
            }
            for (name, value) in req.headers {
                w.header(name, value);
            }
            w.finish().map_err(|_| Error::BadRequest)? as u32
        };

        let reuse = self.conns[pick].state == State::Idle
            && self.conns[pick].host_id == req.host_id
            && fresh(&self.conns[pick]);
        let was_idle = self.conns[pick].state == State::Idle;
        let c = &mut self.conns[pick];
        c.token = token;
        c.started_at = now;
        c.head_len = head_len;
        c.body = req.body;
        c.response_buf = req.response_buf;
        c.is_head = req.method == "HEAD";
        c.timed_out = false;
        // The name the certificate must match is the host alone: a Host value
        // may carry a port, a server name may not.
        let name = req.host.split(':').next().unwrap_or(req.host);
        c.name[..name.len()].copy_from_slice(name.as_bytes());
        c.name_len = name.len() as u8;
        if reuse {
            // The check reads into the free space after the request head. A
            // head that fills the buffer leaves none, and goes unchecked.
            if self.cfg.check_idle && head_len < self.cfg.head_max {
                c.state = State::CheckIdle;
                let found = self.streams[pick].check_idle(io, self.head_buf(pick), head_len);
                self.after_idle_check(io, pick, found);
            } else {
                self.send_head(io, pick, 0);
            }
        } else {
            c.host_id = req.host_id;
            c.addr = req.addr;
            if was_idle {
                c.state = State::Closing { then_connect: true };
                self.streams[pick].reset();
                self.submit(
                    io,
                    pick,
                    K_CLOSE,
                    Op::Close {
                        sock: self.sock(pick),
                    },
                );
            } else {
                self.connect(io, pick);
            }
        }
        Ok(())
    }

    /// Act on what an idle connection held: use it, or replace it and send
    /// the waiting request on the new one.
    pub(super) fn after_idle_check<IO: Io>(&mut self, io: &mut IO, i: usize, found: IdleCheck) {
        match found {
            IdleCheck::Pending => return,
            IdleCheck::Clean => return self.send_head(io, i, 0),
            IdleCheck::Closed => self.idle_closed += 1,
            IdleCheck::Dirty => self.idle_dirty += 1,
        }
        self.conns[i].state = State::Closing { then_connect: true };
        self.streams[i].reset();
        self.submit(io, i, K_CLOSE, Op::Close { sock: self.sock(i) });
    }

    pub(super) fn connect<IO: Io>(&mut self, io: &mut IO, i: usize) {
        self.connects += 1;
        self.conns[i].state = State::Connecting;
        self.conns[i].uses = 0;
        self.submit(
            io,
            i,
            K_CONNECT,
            Op::Connect {
                sock: self.sock(i),
                addr: self.conns[i].addr,
            },
        );
    }

    pub(super) fn send_head<IO: Io>(&mut self, io: &mut IO, i: usize, off: u32) {
        self.conns[i].state = State::SendHead { off };
        // Heads are small; the kernel would copy a zero-copy send this size anyway.
        self.streams[i].send(
            io,
            self.head_buf(i),
            off,
            self.conns[i].head_len - off,
            u32::MAX,
        );
    }

    pub(super) fn send_body<IO: Io>(&mut self, io: &mut IO, i: usize, off: u32) {
        let (buf, len) = self.conns[i]
            .body
            .expect("send_body is only reached with a body");
        self.conns[i].state = State::SendBody { off };
        self.streams[i].send(io, buf, off, len - off, self.cfg.zero_copy_min);
    }
}
