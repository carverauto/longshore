// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Routing completions to connections, and the periodic timeout and age sweep.
//! What a plaintext result means for a connection is in `plain`.

use super::*;

impl Client {
    /// Feed a completion.
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, c: Completion) -> Handled {
        let Some(rel) = c
            .token()
            .get()
            .checked_sub(self.cfg.token_base())
            .filter(|r| *r < u64::from(self.cfg.connections_max()) * 8)
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
            return self.on_plain(io, i, kind, c.result());
        }
        if state == State::CheckIdle {
            if kind == K_RECV {
                let found = self.streams[i].on_idle_check(io, c.result());
                self.after_idle_check(io, i, found);
            }
            return Handled::Pending;
        }
        // Sends and receives go through the stream, which turns TLS records
        // into plaintext results.
        #[cfg(feature = "tls-core")]
        if state == State::TlsHandshake {
            return match self.streams[i].on_handshake(io, kind == K_SEND, c.result()) {
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
            self.streams[i].on_send(io, c.result())
        } else {
            self.streams[i].on_recv(io, c.result())
        };
        match step {
            Step::Pending => Handled::Pending,
            Step::Done(r) => self.on_plain(io, i, kind, r),
            Step::Tls(e) => self.fail(io, i, Error::Tls(e)),
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
                && now.saturating_sub(self.conns[i].opened_at) >= self.cfg.max_age_ns()
            {
                self.conns[i].state = State::Closing {
                    then_connect: false,
                };
                self.streams[i].reset();
                self.submit(io, i, K_CLOSE, Op::Close { sock: self.sock(i) });
                continue;
            }
            let c = &self.conns[i];
            if matches!(c.state, State::Closed | State::Idle | State::Closing { .. }) {
                continue;
            }
            if c.timed_out || now.saturating_sub(c.started_at) < self.cfg.request_timeout_ns() {
                continue;
            }
            self.cancel_ops(io, i);
        }
    }

    /// Submit cancels for whatever connection `i`'s request has outstanding.
    /// The request then ends with [`Error::Timeout`] when one completes.
    fn cancel_ops<IO: Io>(&mut self, io: &mut IO, i: usize) {
        let c = &mut self.conns[i];
        let kinds: &[u64] = match c.state {
            State::Connecting => &[K_CONNECT],
            State::SendHead { .. } | State::SendBody { .. } => &[K_SEND],
            // Either may be outstanding; cancelling the other finds nothing.
            #[cfg(feature = "tls-core")]
            State::TlsHandshake => &[K_SEND, K_RECV],
            _ => &[K_RECV],
        };
        c.timed_out = true;
        for kind in kinds {
            let target = self.token(i, *kind);
            self.submit(io, i, K_CANCEL, Op::Cancel { target });
        }
    }

    /// The connection carrying the request started with `token`, if it is
    /// still in progress.
    fn request_conn(&self, token: u64) -> Option<usize> {
        self.conns.iter().position(|c| {
            c.token == token
                && !matches!(
                    c.state,
                    State::Closed
                        | State::Idle
                        | State::Closing {
                            then_connect: false
                        }
                )
        })
    }

    /// Whether the head of the request in progress under `token` has been
    /// handed to the `Io` for sending. False if there is no such request.
    pub fn sent(&self, token: u64) -> bool {
        self.request_conn(token).is_some_and(|i| self.conns[i].sent)
    }

    /// Stop the request in progress under `token` before its own timeout.
    ///
    /// [`Handled::NotMine`] if there is no such request. [`Handled::Done`]
    /// if it ended here, which happens only when nothing of it was in the
    /// kernel yet. Otherwise [`Handled::Pending`]: its operations have been
    /// asked to stop, and the request ends through [`Client::on_completion`]
    /// with [`Error::Timeout`] once the kernel has let go of them. If the
    /// response completes first, that completion still reports
    /// [`Error::Timeout`]: a cancelled request has no result.
    pub fn cancel<IO: Io>(&mut self, io: &mut IO, token: u64) -> Handled {
        let Some(i) = self.request_conn(token) else {
            return Handled::NotMine;
        };
        if self.conns[i].state == (State::Closing { then_connect: true }) {
            // Only the old connection's close is outstanding: the request
            // has not begun, and now never will.
            self.conns[i].state = State::Closing {
                then_connect: false,
            };
            return Handled::Done(Done::new(token, Err(Error::Timeout), false));
        }
        if !self.conns[i].timed_out {
            self.cancel_ops(io, i);
        }
        Handled::Pending
    }
}
