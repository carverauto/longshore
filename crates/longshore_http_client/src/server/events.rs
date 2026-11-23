// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Parsing request heads and routing completions.

use super::*;

impl Server {
    pub(super) fn on_head_bytes<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) {
        let parsed = parse_request(
            &io.buf(self.head_buf(i))[..have as usize],
            self.cfg.head_max() as usize,
        );
        let head = match parsed {
            Err(_) => return self.status(io, i, 400, "Bad Request", 0, 0, true),
            Ok(None) => return self.recv_head(io, i, have),
            Ok(Some(h)) => h,
        };
        let head_len = head.fields().head_len() as u32;
        let body_len = head.fields().content_length().unwrap_or(0);
        let extra = have - head_len;
        let too_long = self.note_request(i, io.buf(self.head_buf(i)), &head);
        if too_long
            || head.fields().chunked()
            || body_len > u64::from(self.cfg.buffer_size())
            || u64::from(extra) > body_len
        {
            return self.status(io, i, 400, "Bad Request", 0, 0, true);
        }
        self.conns[i].request_len = body_len as u32;
        self.move_early_body(io, i, head_len, extra);
        if u64::from(extra) < body_len {
            self.conns[i].state = State::RecvBody { have: extra };
            self.recv_into(io, i, self.body_buf(i), extra, body_len as u32 - extra);
        } else {
            self.handle(io, i);
        }
    }

    /// Feed a completion. Returns false if the token is not the server's.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> bool {
        let Some(rel) = c
            .token()
            .get()
            .checked_sub(self.cfg.token_base())
            .filter(|r| *r <= u64::from(self.cfg.connections_max()) * 8)
        else {
            return false;
        };
        if c.token().get() == self.accept_token() {
            self.accepting = false;
            #[expect(
                clippy::expect_used,
                reason = "an accept completes only for the slot accept() put in Accepting"
            )]
            let i = self
                .conns
                .iter()
                .position(|conn| conn.state == State::Accepting)
                .expect("an accept was in flight");
            if c.result() < 0 {
                self.conns[i].state = State::Free;
            } else {
                self.accepted += 1;
                #[cfg(not(feature = "tls"))]
                self.recv_head(io, i, 0);
                #[cfg(feature = "tls")]
                match &self.tls {
                    None => self.recv_head(io, i, 0),
                    Some(setup) => {
                        self.conns[i].state = State::Handshake;
                        let started = Handshake::server(setup)
                            .map(|h| self.streams[i].start_handshake(io, h));
                        if !matches!(started, Ok(Step::Pending)) {
                            self.close(io, i);
                        }
                    }
                }
            }
            self.accept(io);
            return true;
        }
        let (i, kind) = ((rel / 8) as usize, rel % 8);
        if kind == K_CLOSE {
            self.on_plain(io, i, kind, c.result());
            return true;
        }
        // Sends and receives go through the stream.
        #[cfg(feature = "tls")]
        if self.conns[i].state == State::Handshake {
            match self.streams[i].on_handshake(io, kind == K_SEND, c.result()) {
                Step::Pending => {}
                Step::Done(0) => self.recv_head(io, i, 0),
                Step::Done(_) | Step::Tls(_) => self.close(io, i),
            }
            return true;
        }
        let step = if kind == K_SEND {
            self.streams[i].on_send(io, c.result())
        } else {
            self.streams[i].on_recv(io, c.result())
        };
        match step {
            Step::Pending => {}
            Step::Done(r) => self.on_plain(io, i, kind, r),
            Step::Tls(_) => self.close(io, i),
        }
        true
    }

    /// Handle a completion in plaintext terms.
    pub(super) fn on_plain<IO: Io>(&mut self, io: &mut IO, i: usize, kind: u64, result: i32) {
        let c = Completion::new(Token::new(0), result);
        match (kind, self.conns[i].state) {
            (K_CLOSE, _) => {
                self.closed += 1;
                self.conns[i].state = State::Free;
                self.accept(io);
            }
            (K_RECV, State::RecvHead { have }) => {
                if c.result() <= 0 {
                    self.close(io, i);
                } else {
                    self.on_head_bytes(io, i, have + c.result().unsigned_abs());
                }
            }
            (K_RECV, State::RecvBody { have }) => self.on_body_bytes(io, i, have, c.result()),
            (K_RECV, State::Stalled) => self.close(io, i),
            (K_SEND, State::SendHead { off }) => self.on_head_sent(io, i, off, c.result()),
            (K_SEND, State::SendBody { off }) => self.on_body_sent(io, i, off, c.result()),
            _ => {}
        }
    }

    /// Body bytes that arrived with the head move to the body buffer.
    fn move_early_body<IO: Io>(&self, io: &mut IO, i: usize, head_len: u32, extra: u32) {
        let mut tmp = [0u8; 1024];
        let mut moved = 0usize;
        while moved < extra as usize {
            let n = (extra as usize - moved).min(tmp.len());
            let from = head_len as usize + moved;
            tmp[..n].copy_from_slice(&io.buf(self.head_buf(i))[from..from + n]);
            io.buf(self.body_buf(i))[moved..moved + n].copy_from_slice(&tmp[..n]);
            moved += n;
        }
    }

    /// A receive of the request body, which had `have` bytes, returned `result`.
    fn on_body_bytes<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32, result: i32) {
        if result <= 0 {
            return self.close(io, i);
        }
        let have = have + result.unsigned_abs();
        if have < self.conns[i].request_len {
            self.conns[i].state = State::RecvBody { have };
            self.recv_into(
                io,
                i,
                self.body_buf(i),
                have,
                self.conns[i].request_len - have,
            );
        } else {
            self.handle(io, i);
        }
    }

    /// A send of the response head from `off` returned `result`.
    fn on_head_sent<IO: Io>(&mut self, io: &mut IO, i: usize, off: u32, result: i32) {
        if result <= 0 {
            return self.close(io, i);
        }
        let off = off + result.unsigned_abs();
        let conn = self.conns[i];
        if off < conn.head_len {
            self.conns[i].state = State::SendHead { off };
            self.send_from(io, i, self.head_buf(i), off, conn.head_len - off);
        } else if conn.body_len > 0 {
            self.conns[i].state = State::SendBody { off: 0 };
            self.send_from(io, i, self.body_buf(i), 0, conn.body_len);
        } else if conn.close_after {
            self.close(io, i);
        } else {
            self.recv_head(io, i, 0);
        }
    }

    /// A send of the response body from `off` returned `result`.
    fn on_body_sent<IO: Io>(&mut self, io: &mut IO, i: usize, off: u32, result: i32) {
        if result <= 0 {
            return self.close(io, i);
        }
        let off = off + result.unsigned_abs();
        let conn = self.conns[i];
        if off < conn.body_len {
            self.conns[i].state = State::SendBody { off };
            self.send_from(io, i, self.body_buf(i), off, conn.body_len - off);
        } else if conn.close_after {
            self.close(io, i);
        } else {
            self.recv_head(io, i, 0);
        }
    }
}
