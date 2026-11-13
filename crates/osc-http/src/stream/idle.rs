// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Checking a connection that has been idle, before it carries another request.

use super::*;
use osc_io::EAGAIN;

/// What a connection that should be silent turned out to hold.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum IdleCheck {
    /// A read that does not wait was submitted; pass its completion to
    /// [`Stream::on_idle_check`].
    Pending,
    /// Nothing unexpected has arrived. The connection can be used again.
    Clean,
    /// The peer has closed the connection, or it has failed, or what has
    /// arrived cannot be told apart from that.
    Closed,
    /// The peer sent bytes nobody asked for: more than the last response
    /// declared. Nothing on this connection can be trusted to be in step.
    Dirty,
}

impl Stream {
    /// Look at an idle connection without waiting. On a plain connection one
    /// byte of `scratch` at `off` is used as the place to read into.
    pub fn check_idle<IO: Io>(&mut self, io: &mut IO, scratch: BufId, off: u32) -> IdleCheck {
        let (sock, token) = (self.sock, self.recv_token);
        match self.tls.as_mut().filter(|t| t.active) {
            None => {
                Self::submit(
                    io,
                    token,
                    Op::RecvNow {
                        sock,
                        buf: scratch,
                        off,
                        len: 1,
                    },
                );
                IdleCheck::Pending
            }
            Some(t) => t.idle_step(io, sock, token),
        }
    }

    /// The read submitted by [`Stream::check_idle`] completed with `result`.
    pub fn on_idle_check<IO: Io>(&mut self, io: &mut IO, result: i32) -> IdleCheck {
        let (sock, token) = (self.sock, self.recv_token);
        let Some(t) = self.tls.as_mut().filter(|t| t.active) else {
            return match result {
                r if r == -EAGAIN => IdleCheck::Clean,
                r if r > 0 => IdleCheck::Dirty,
                _ => IdleCheck::Closed,
            };
        };
        if result == -EAGAIN {
            // Nothing more has arrived. Part of a record left waiting is not
            // silence: what it will turn out to be is unknown.
            return if t.rx_start == t.rx_end {
                IdleCheck::Clean
            } else {
                IdleCheck::Closed
            };
        }
        if result <= 0 {
            return IdleCheck::Closed;
        }
        t.rx_end += result as u32;
        t.idle_step(io, sock, token)
    }
}

impl Tls {
    /// Open every complete record already received, then read again without
    /// waiting. Session tickets are expected at any time and are skipped.
    /// Application data is not, and an alert ends the connection.
    fn idle_step<IO: Io>(&mut self, io: &mut IO, sock: Sock, token: Token) -> IdleCheck {
        if self.spill_len > 0 {
            return IdleCheck::Dirty;
        }
        if self.closed {
            return IdleCheck::Closed;
        }
        loop {
            let avail = &mut io.buf(self.rx_buf)[self.rx_start as usize..self.rx_end as usize];
            let len = match record_len(avail) {
                Err(_) => return IdleCheck::Closed,
                Ok(Some(len)) if avail.len() >= len => len,
                Ok(_) => break,
            };
            match self.rx.open_in_place(&mut avail[..len]) {
                Ok((CONTENT_HANDSHAKE, _)) => {}
                // A record of padding alone carries nothing.
                Ok((CONTENT_APPLICATION_DATA, 0)) => {}
                Ok((CONTENT_APPLICATION_DATA, _)) => return IdleCheck::Dirty,
                // An alert, a failed record, or a type that should not exist.
                _ => return IdleCheck::Closed,
            }
            self.rx_start += len as u32;
        }
        if self.make_room(io).is_err() {
            return IdleCheck::Closed;
        }
        io.submit(
            token,
            Op::RecvNow {
                sock,
                buf: self.rx_buf,
                off: self.rx_end,
                len: self.cap - self.rx_end,
            },
        )
        .expect("Limits::ops_max must cover one receive per connection");
        IdleCheck::Pending
    }
}
