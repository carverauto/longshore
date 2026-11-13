// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Routing completions to connections, and the periodic timeout and age sweep.

use super::*;

impl Client {
    /// Feed a completion.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> Handled {
        let Some(rel) = c
            .token
            .0
            .checked_sub(self.cfg.token_base)
            .filter(|r| *r < self.cfg.connections_max as u64 * 8)
        else {
            return Handled::NotMine;
        };
        let (i, kind) = ((rel / 8) as usize, rel % 8);
        let state = self.conns[i].state;
        let busy = !matches!(state, State::Closed | State::Idle | State::Closing { .. });
        // A request past its deadline ends here, whatever its operation
        // returned: a cancelled operation may still complete successfully.
        if busy && self.conns[i].timed_out && matches!(kind, K_CONNECT | K_SEND | K_RECV) {
            return self.fail(io, i, Error::Timeout);
        }
        if !matches!(kind, K_SEND | K_RECV) || !busy {
            return self.on_plain(io, i, kind, c.result);
        }
        if state == State::CheckIdle {
            if kind == K_RECV {
                let found = self.streams[i].on_idle_check(io, c.result);
                self.after_idle_check(io, i, found);
            }
            return Handled::Pending;
        }
        // Sends and receives go through the stream, which turns TLS records
        // into plaintext results.
        if state == State::TlsHandshake {
            return match self.streams[i].on_handshake(io, kind == K_SEND, c.result) {
                Step::Pending => Handled::Pending,
                Step::Done(0) => {
                    self.send_head(io, i, 0);
                    Handled::Pending
                }
                Step::Done(r) => self.fail(
                    io,
                    i,
                    if r == -ECANCELED {
                        Error::Timeout
                    } else {
                        Error::Io(-r)
                    },
                ),
                Step::Tls(e) => self.fail(io, i, Error::Tls(e)),
            };
        }
        let step = if kind == K_SEND {
            self.streams[i].on_send(io, c.result)
        } else {
            self.streams[i].on_recv(io, c.result)
        };
        match step {
            Step::Pending => Handled::Pending,
            Step::Done(r) => self.on_plain(io, i, kind, r),
            Step::Tls(e) => self.fail(io, i, Error::Tls(e)),
        }
    }

    /// Handle a completion in terms of plaintext: `result` is what a plain
    /// socket would have returned.
    pub(super) fn on_plain<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        kind: u64,
        result: i32,
    ) -> Handled {
        let c = Completion {
            token: self.token(i, kind),
            result,
        };
        let state = self.conns[i].state;
        let reused = self.conns[i].uses > 0;
        let io_error = |r: i32| Error::Io(-r);
        let in_progress = matches!(
            (kind, state),
            (K_CONNECT, State::Connecting)
                | (K_SEND, State::SendHead { .. } | State::SendBody { .. })
                | (K_RECV, State::RecvHead { .. } | State::RecvBody { .. })
        );
        if in_progress && self.conns[i].timed_out {
            return self.fail(io, i, Error::Timeout);
        }
        match (kind, state) {
            (K_CANCEL, _) => Handled::Pending,
            (K_CLOSE, State::Closing { then_connect }) => {
                if then_connect {
                    self.connect(io, i);
                } else {
                    self.conns[i].state = State::Closed;
                }
                Handled::Pending
            }
            (K_CONNECT, State::Connecting) => {
                if c.result < 0 {
                    // The I/O layer already freed the slot of a failed
                    // connect; the close `fail` submits is redundant but harmless.
                    return self.fail(io, i, io_error(c.result));
                }
                self.conns[i].opened_at = io.now_ns();
                self.conns[i].segment = c.result as u32;
                if c.result > 0 {
                    self.connected = Some((self.conns[i].addr, c.result as u32));
                }
                let Some(setup) = &self.tls else {
                    self.send_head(io, i, 0);
                    return Handled::Pending;
                };
                let conn = &self.conns[i];
                let name = core::str::from_utf8(&conn.name[..conn.name_len as usize]).unwrap_or("");
                let handshake = match Handshake::client(setup, name) {
                    Ok(h) => h,
                    Err(e) => return self.fail(io, i, Error::Tls(e)),
                };
                self.conns[i].state = State::TlsHandshake;
                match self.streams[i].start_handshake(io, handshake) {
                    Step::Tls(e) => self.fail(io, i, Error::Tls(e)),
                    _ => Handled::Pending,
                }
            }
            (K_SEND, State::SendHead { off }) => {
                if c.result <= 0 {
                    return self.fail(
                        io,
                        i,
                        if c.result == 0 {
                            Error::Closed { reused }
                        } else {
                            io_error(c.result)
                        },
                    );
                }
                let off = off + c.result as u32;
                if off < self.conns[i].head_len {
                    self.send_head(io, i, off);
                } else if self.conns[i].body.is_some_and(|b| b.1 > 0) {
                    self.send_body(io, i, 0);
                } else {
                    return self.recv_head(io, i, 0);
                }
                Handled::Pending
            }
            (K_SEND, State::SendBody { off }) => {
                if c.result <= 0 {
                    return self.fail(
                        io,
                        i,
                        if c.result == 0 {
                            Error::Closed { reused }
                        } else {
                            io_error(c.result)
                        },
                    );
                }
                let off = off + c.result as u32;
                if off < self.conns[i].body.map_or(0, |b| b.1) {
                    self.send_body(io, i, off);
                    Handled::Pending
                } else {
                    self.recv_head(io, i, 0)
                }
            }
            (K_RECV, State::RecvHead { have }) => match c.result {
                0 => self.fail(io, i, Error::Closed { reused }),
                r if r < 0 => self.fail(io, i, io_error(r)),
                r => self.on_head_bytes(io, i, have + r as u32),
            },
            (K_RECV, State::RecvBody { have }) => match c.result {
                0 => self.fail(io, i, Error::Closed { reused }),
                r if r < 0 => self.fail(io, i, io_error(r)),
                r => {
                    let have = have + r as u32;
                    if have == self.conns[i].body_len {
                        self.succeed(io, i)
                    } else {
                        self.recv_body(io, i, have)
                    }
                }
            },
            (K_RECV, State::Drain { have }) => match c.result {
                0 => self.fail(io, i, Error::Closed { reused }),
                r if r < 0 => self.fail(io, i, io_error(r)),
                r => {
                    let have = have + r as u32;
                    if have == self.conns[i].body_len {
                        self.succeed(io, i)
                    } else {
                        self.drain(io, i, have)
                    }
                }
            },
            (K_RECV, State::RecvChunked { decoded }) => match c.result {
                0 => self.fail(io, i, Error::Closed { reused }),
                r if r < 0 => self.fail(io, i, io_error(r)),
                r => self.on_chunked_bytes(io, i, decoded, decoded + r as u32),
            },
            (K_RECV, State::RecvChunkedTail) => match c.result {
                0 => self.fail(io, i, Error::Closed { reused }),
                r if r < 0 => self.fail(io, i, io_error(r)),
                r => self.on_chunked_tail(io, i, r as u32),
            },
            // A completion that no longer matches the connection's state.
            _ => Handled::Pending,
        }
    }

    /// Cancel requests that have run past the timeout, and close idle
    /// connections that have passed their maximum age. Call periodically; a
    /// cancelled request then completes with [`Error::Timeout`].
    pub fn tick<IO: Io>(&mut self, io: &mut IO) {
        let now = io.now_ns();
        for i in 0..self.conns.len() {
            // Aged connections are closed on our own schedule, whether or
            // not a request happens to need the slot.
            if self.conns[i].state == State::Idle
                && now.saturating_sub(self.conns[i].opened_at) >= self.cfg.max_age_ns
            {
                self.conns[i].state = State::Closing {
                    then_connect: false,
                };
                self.streams[i].reset();
                self.submit(io, i, K_CLOSE, Op::Close { sock: self.sock(i) });
                continue;
            }
            let c = &mut self.conns[i];
            let kinds: &[u64] = match c.state {
                State::Connecting => &[K_CONNECT],
                State::SendHead { .. } | State::SendBody { .. } => &[K_SEND],
                State::RecvHead { .. }
                | State::RecvBody { .. }
                | State::Drain { .. }
                | State::RecvChunked { .. }
                | State::RecvChunkedTail
                | State::CheckIdle => &[K_RECV],
                // Either may be outstanding; cancelling the other finds nothing.
                State::TlsHandshake => &[K_SEND, K_RECV],
                _ => continue,
            };
            if c.timed_out || now.saturating_sub(c.started_at) < self.cfg.request_timeout_ns {
                continue;
            }
            c.timed_out = true;
            for kind in kinds {
                let target = self.token(i, *kind);
                self.submit(io, i, K_CANCEL, Op::Cancel { target });
            }
        }
    }
}
