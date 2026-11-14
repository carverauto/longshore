// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving a response: the head, a body with a declared length, and how a request ends.

use super::*;

impl Client {
    /// A receive was requested; with TLS it may already be satisfied from
    /// records that arrived earlier.
    pub(super) fn after_recv<IO: Io>(&mut self, io: &mut IO, i: usize, step: Step) -> Handled {
        match step {
            Step::Pending => Handled::Pending,
            Step::Done(r) => self.on_plain(io, i, K_RECV, r),
            Step::Tls(e) => self.fail(io, i, Error::Tls(e)),
        }
    }

    pub(super) fn recv_head<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) -> Handled {
        self.conns[i].state = State::RecvHead { have };
        let step = self.streams[i].recv(io, self.head_buf(i), have, self.cfg.head_max - have);
        self.after_recv(io, i, step)
    }

    pub(super) fn recv_body<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) -> Handled {
        let buf = self.conns[i]
            .response_buf
            .expect("recv_body is only reached with a response buffer");
        self.conns[i].state = State::RecvBody { have };
        let step = self.streams[i].recv(io, buf, have, self.conns[i].body_len - have);
        self.after_recv(io, i, step)
    }

    /// Read more of a body that is being discarded, over the bytes after the head.
    pub(super) fn drain<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) -> Handled {
        let c = &self.conns[i];
        let room = self.cfg.head_max - c.head_len;
        // Fill the room after the head in order; once it is full, later
        // bytes overwrite it. A body that fits stays readable afterwards.
        let used = have.min(room);
        let (off, len) = if used < room {
            (c.head_len + used, (c.body_len - have).min(room - used))
        } else {
            (c.head_len, (c.body_len - have).min(room))
        };
        self.conns[i].state = State::Drain { have };
        let step = self.streams[i].recv(io, self.head_buf(i), off, len);
        self.after_recv(io, i, step)
    }

    /// The request failed: close the connection and report.
    pub(super) fn fail<IO: Io>(&mut self, io: &mut IO, i: usize, e: Error) -> Handled {
        let c = &mut self.conns[i];
        if c.state == State::Connecting {
            self.connect_failure = Some(c.addr);
        }
        c.state = State::Closing {
            then_connect: false,
        };
        let token = c.token;
        self.streams[i].reset();
        self.submit(io, i, K_CLOSE, Op::Close { sock: self.sock(i) });
        Handled::Done(Done {
            token,
            result: Err(e),
        })
    }

    pub(super) fn succeed<IO: Io>(&mut self, io: &mut IO, i: usize) -> Handled {
        // Over TLS, bytes sent past the declared body may already be decrypted
        // and waiting. Left there they would be read as the next response.
        if self.streams[i].has_unread_plaintext() {
            return self.fail(io, i, Error::Head(HttpError::BadFraming));
        }
        let c = &mut self.conns[i];
        c.uses += 1;
        let done = Done {
            token: c.token,
            result: Ok(Response {
                status: c.status,
                body_len: c.body_len,
                conn: i as u32,
            }),
        };
        // Close when the server asked to, and when the traffic keys have done
        // enough work: the next request gets a new connection and new keys.
        if c.fields.is_some_and(|f| f.close)
            || self.streams[i].records() >= self.cfg.tls_records_max
        {
            c.state = State::Closing {
                then_connect: false,
            };
            self.streams[i].reset();
            self.submit(io, i, K_CLOSE, Op::Close { sock: self.sock(i) });
        } else {
            c.state = State::Idle;
        }
        Handled::Done(done)
    }

    /// The head buffer holds `have` bytes; parse and move on.
    pub(super) fn on_head_bytes<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        mut have: u32,
    ) -> Handled {
        loop {
            let parsed = parse_response(
                &io.buf(self.head_buf(i))[..have as usize],
                self.cfg.head_max as usize,
            );
            let head = match parsed {
                Err(e) => return self.fail(io, i, Error::Head(e)),
                Ok(None) => return self.recv_head(io, i, have),
                Ok(Some(h)) => h,
            };
            let head_len = head.fields.head_len as u32;
            // 101 is not interim: what follows it is another protocol, which
            // this client never asks for and cannot read.
            if head.status == 101 {
                return self.fail(io, i, Error::Head(HttpError::BadFraming));
            }
            if (100..200).contains(&head.status) {
                // An interim response has no body and does not end the
                // connection. One that claims either cannot be skipped
                // safely: its "body" would be read as the real response.
                if head.fields.chunked
                    || head.fields.content_length.is_some_and(|n| n > 0)
                    || head.fields.close
                {
                    return self.fail(io, i, Error::Head(HttpError::BadFraming));
                }
                // Interim response (e.g. 100 Continue): drop it and parse what follows.
                io.buf(self.head_buf(i))
                    .copy_within(head_len as usize..have as usize, 0);
                have -= head_len;
                continue;
            }
            let c = &mut self.conns[i];
            c.status = head.status;
            c.fields = Some(head.fields);
            // From here `head_len` is the response head's length; the request head is gone.
            c.head_len = head_len;
            let no_body = c.is_head || matches!(head.status, 204 | 304);
            if !no_body && head.fields.chunked {
                return self.start_chunked(io, i, head_len, have);
            }
            if !no_body && head.fields.content_length.is_none() {
                return self.fail(io, i, Error::UnsupportedFraming);
            }
            let body_len = if no_body {
                0
            } else {
                head.fields.content_length.unwrap_or(0)
            };
            let extra = have - head_len;
            if extra as u64 > body_len {
                // Bytes beyond the declared body: the framing cannot be trusted.
                return self.fail(io, i, Error::Head(HttpError::BadFraming));
            }
            if body_len == 0 {
                c.body_len = 0;
                return self.succeed(io, i);
            }
            let Some(dst) = c.response_buf else {
                // No buffer: discard the body so the connection stays usable.
                let room = self.cfg.head_max - head_len;
                if body_len > DRAIN_MAX || room == 0 {
                    return self.fail(io, i, Error::BodyTooLarge);
                }
                c.body_len = body_len as u32;
                if extra as u64 == body_len {
                    return self.succeed(io, i);
                }
                return self.drain(io, i, extra);
            };
            if body_len > self.cfg.buffer_size as u64 {
                return self.fail(io, i, Error::BodyTooLarge);
            }
            c.body_len = body_len as u32;
            // Move the body bytes that arrived with the head. This is the only
            // copy of response data, and it is bounded by the head buffer.
            let mut tmp = [0u8; 1024];
            let mut moved = 0usize;
            while moved < extra as usize {
                let n = (extra as usize - moved).min(tmp.len());
                let from = head_len as usize + moved;
                tmp[..n].copy_from_slice(&io.buf(self.head_buf(i))[from..from + n]);
                io.buf(dst)[moved..moved + n].copy_from_slice(&tmp[..n]);
                moved += n;
            }
            if extra as u64 == body_len {
                return self.succeed(io, i);
            }
            return self.recv_body(io, i, extra);
        }
    }
}
