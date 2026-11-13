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
            self.cfg.head_max as usize,
        );
        let head = match parsed {
            Err(_) => return self.status(io, i, 400, "Bad Request", 0, 0, true),
            Ok(None) => return self.recv_head(io, i, have),
            Ok(Some(h)) => h,
        };
        let head_len = head.fields.head_len as u32;
        let body_len = head.fields.content_length.unwrap_or(0);
        let extra = have - head_len;
        let (allowed_keys, allowed_count) = (self.allowed_keys, self.allowed_count);
        let bucket = self.bucket.as_bytes();
        let too_long = {
            let buf = io.buf(self.head_buf(i));
            let (method, target) = (head.method(buf), head.target(buf));
            // With the bucket in the path, "/bucket/rest" is served as "/rest".
            let target = target
                .strip_prefix(b"/")
                .and_then(|t| t.strip_prefix(bucket))
                .filter(|rest| !bucket.is_empty() && rest.first() == Some(&b'/'))
                .unwrap_or(target);
            let c = &mut self.conns[i];
            let target_max = if method == b"GET" && target.starts_with(b"/?") {
                LIST_TARGET_MAX
            } else {
                TARGET_MAX
            };
            let too_long = target.len() > target_max || method.len() > c.method.len();
            if !too_long {
                c.target[..target.len()].copy_from_slice(target);
                c.target_len = target.len();
                c.method[..method.len()].copy_from_slice(method);
                c.method_len = method.len();
            }
            c.close_after = head.fields.close;
            c.if_none_match = head.fields.header(buf, "if-none-match") == Some(b"*")
                || head.fields.header(buf, "x-goog-if-generation-match") == Some(b"0");
            c.claimed = head
                .fields
                .header(buf, "x-amz-checksum-crc64nvme")
                .and_then(|v| v.try_into().ok())
                .or_else(|| {
                    let v = head
                        .fields
                        .header(buf, "x-goog-hash")?
                        .strip_prefix(b"crc32c=")?;
                    let mut sum = [0u8; 12];
                    sum[..8].copy_from_slice(v.get(..8).filter(|_| v.len() == 8)?);
                    Some(sum)
                });
            c.reply_checksum = None;
            c.reply_tag = None;
            c.reply_object = None;
            c.note_describing(&buf[..head_len as usize]);
            let source = head
                .fields
                .header(buf, "x-amz-copy-source")
                .or_else(|| head.fields.header(buf, "x-goog-copy-source"))
                .filter(|v| v.len() <= TARGET_MAX)
                .unwrap_or(b"");
            c.copy_source[..source.len()].copy_from_slice(source);
            c.copy_source_len = source.len();
            c.checksum_mode = head.fields.header(buf, "x-amz-checksum-mode") == Some(b"ENABLED");
            c.bearer = head
                .fields
                .header(buf, "authorization")
                .is_some_and(|v| v.starts_with(b"Bearer "));
            c.authorized = allowed_count == 0
                || head.fields.header(buf, "authorization").is_some_and(|v| {
                    // "... Credential=<access key>/<date>/...", or "Bearer <token>".
                    let key = v.strip_prefix(b"Bearer ").or_else(|| {
                        v.windows(11)
                            .position(|w| w == b"Credential=")
                            .map(|at| &v[at + 11..])
                            .and_then(|rest| rest.split(|b| *b == b'/').next())
                    });
                    key.is_some_and(|k| {
                        allowed_keys[..allowed_count]
                            .iter()
                            .any(|(a, n)| &a[..*n] == k)
                    })
                });
            let (token, token_len) = self.session_token;
            if token_len > 0 {
                c.authorized &=
                    head.fields.header(buf, "x-amz-security-token") == Some(&token[..token_len]);
            }
            c.range = head.fields.header(buf, "range").and_then(|v| {
                let v = core::str::from_utf8(v).ok()?.strip_prefix("bytes=")?;
                let (a, b) = v.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            });
            too_long
        };
        if too_long
            || head.fields.chunked
            || body_len > self.cfg.buffer_size as u64
            || extra as u64 > body_len
        {
            return self.status(io, i, 400, "Bad Request", 0, 0, true);
        }
        self.conns[i].request_len = body_len as u32;
        // Body bytes that arrived with the head move to the body buffer.
        let mut tmp = [0u8; 1024];
        let mut moved = 0usize;
        while moved < extra as usize {
            let n = (extra as usize - moved).min(tmp.len());
            let from = head_len as usize + moved;
            tmp[..n].copy_from_slice(&io.buf(self.head_buf(i))[from..from + n]);
            io.buf(self.body_buf(i))[moved..moved + n].copy_from_slice(&tmp[..n]);
            moved += n;
        }
        if (extra as u64) < body_len {
            self.conns[i].state = State::RecvBody { have: extra };
            self.recv_into(io, i, self.body_buf(i), extra, body_len as u32 - extra);
        } else {
            self.handle(io, i);
        }
    }

    /// Feed a completion. Returns false if the token is not the server's.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> bool {
        let Some(rel) = c
            .token
            .0
            .checked_sub(self.cfg.token_base)
            .filter(|r| *r <= self.cfg.connections_max as u64 * 8)
        else {
            return false;
        };
        if c.token.0 == self.accept_token() {
            self.accepting = false;
            let i = self
                .conns
                .iter()
                .position(|c| c.state == State::Accepting)
                .expect("an accept was in flight");
            if c.result < 0 {
                self.conns[i].state = State::Free;
            } else {
                self.accepted += 1;
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
            self.on_plain(io, i, kind, c.result);
            return true;
        }
        // Sends and receives go through the stream.
        if self.conns[i].state == State::Handshake {
            match self.streams[i].on_handshake(io, kind == K_SEND, c.result) {
                Step::Pending => {}
                Step::Done(0) => self.recv_head(io, i, 0),
                _ => self.close(io, i),
            }
            return true;
        }
        let step = if kind == K_SEND {
            self.streams[i].on_send(io, c.result)
        } else {
            self.streams[i].on_recv(io, c.result)
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
        let c = Completion {
            token: Token(0),
            result,
        };
        match (kind, self.conns[i].state) {
            (K_CLOSE, _) => {
                self.closed += 1;
                self.conns[i].state = State::Free;
                self.accept(io);
            }
            (K_RECV, State::RecvHead { have }) => {
                if c.result <= 0 {
                    self.close(io, i);
                } else {
                    self.on_head_bytes(io, i, have + c.result as u32);
                }
            }
            (K_RECV, State::RecvBody { have }) => {
                if c.result <= 0 {
                    self.close(io, i);
                    return;
                }
                let have = have + c.result as u32;
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
            (K_RECV, State::Stalled) => self.close(io, i),
            (K_SEND, State::SendHead { off }) => {
                if c.result <= 0 {
                    self.close(io, i);
                    return;
                }
                let off = off + c.result as u32;
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
            (K_SEND, State::SendBody { off }) => {
                if c.result <= 0 {
                    self.close(io, i);
                    return;
                }
                let off = off + c.result as u32;
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
            _ => {}
        }
    }
}
