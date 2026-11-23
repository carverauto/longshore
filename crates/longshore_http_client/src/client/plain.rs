// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a completion, in plaintext terms, means for the connection it is on.

use super::*;

impl Client {
    /// Handle a completion in terms of plaintext: `result` is what a plain
    /// socket would have returned.
    pub(super) fn on_plain<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        kind: u64,
        result: i32,
    ) -> Handled {
        let state = self.conns[i].state;
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
            (K_CLOSE, State::Closing { then_connect }) => self.on_closed(io, i, then_connect),
            (K_CONNECT, State::Connecting) => self.on_connected(io, i, result),
            (K_SEND, State::SendHead { off }) => match self.moved(i, result) {
                Ok(n) => self.on_head_sent(io, i, off + n),
                Err(e) => self.fail(io, i, e),
            },
            (K_SEND, State::SendBody { off }) => match self.moved(i, result) {
                Ok(n) => self.on_body_sent(io, i, off + n),
                Err(e) => self.fail(io, i, e),
            },
            (K_RECV, State::RecvHead { have }) => match self.moved(i, result) {
                Ok(n) => self.on_head_bytes(io, i, have + n),
                Err(e) => self.fail(io, i, e),
            },
            (K_RECV, State::RecvBody { have }) => match self.moved(i, result) {
                Ok(n) => self.on_body_bytes(io, i, have + n),
                Err(e) => self.fail(io, i, e),
            },
            (K_RECV, State::Drain { have }) => match self.moved(i, result) {
                Ok(n) => self.on_drained(io, i, have + n),
                Err(e) => self.fail(io, i, e),
            },
            (K_RECV, State::RecvChunked { decoded }) => match self.moved(i, result) {
                Ok(n) => self.on_chunked_bytes(io, i, decoded, decoded + n),
                Err(e) => self.fail(io, i, e),
            },
            (K_RECV, State::RecvChunkedTail) => match self.moved(i, result) {
                Ok(n) => self.on_chunked_tail(io, i, n),
                Err(e) => self.fail(io, i, e),
            },
            // A completion that no longer matches the connection's state.
            _ => Handled::Pending,
        }
    }

    /// The bytes a send or receive on connection `i` moved, or why it failed:
    /// a result of zero means the peer closed the connection.
    #[inline]
    fn moved(&self, i: usize, result: i32) -> Result<u32, Error> {
        match result {
            0 => Err(Error::Closed {
                reused: self.conns[i].uses > 0,
            }),
            r if r < 0 => Err(Error::Io(-r)),
            r => Ok(r.unsigned_abs()),
        }
    }

    fn on_closed<IO: Io>(&mut self, io: &mut IO, i: usize, then_connect: bool) -> Handled {
        if then_connect {
            self.connect(io, i);
        } else {
            self.conns[i].state = State::Closed;
        }
        Handled::Pending
    }

    fn on_connected<IO: Io>(&mut self, io: &mut IO, i: usize, result: i32) -> Handled {
        if result < 0 {
            // The I/O layer already freed the slot of a failed
            // connect; the close `fail` submits is redundant but harmless.
            return self.fail(io, i, Error::Io(-result));
        }
        self.conns[i].opened_at = io.now_ns();
        let segment = Segment::decode(result);
        self.conns[i].segment = segment.map_or(0, |s| s.size());
        if let Some(segment) = segment {
            self.connected = Some((self.conns[i].addr, segment));
        }
        #[cfg(not(feature = "tls"))]
        {
            self.send_head(io, i, 0);
            Handled::Pending
        }
        #[cfg(feature = "tls")]
        {
            let Some(setup) = &self.tls else {
                self.send_head(io, i, 0);
                return Handled::Pending;
            };
            let conn = &self.conns[i];
            let name = core::str::from_utf8(&conn.name[..conn.name_len as usize]).unwrap_or("");
            let handshake = match Handshake::client(setup, name) {
                Ok(h) => h,
                Err(e) => return self.fail(io, i, Error::Tls(e.into())),
            };
            self.conns[i].state = State::TlsHandshake;
            match self.streams[i].start_handshake(io, handshake) {
                Step::Tls(e) => self.fail(io, i, Error::Tls(e)),
                Step::Pending | Step::Done(_) => Handled::Pending,
            }
        }
    }

    /// `off` bytes of the request head have been sent.
    fn on_head_sent<IO: Io>(&mut self, io: &mut IO, i: usize, off: u32) -> Handled {
        if off < self.conns[i].head_len {
            self.send_head(io, i, off);
        } else if self.conns[i].body.is_some_and(|b| b.1 > 0) {
            self.send_body(io, i, 0);
        } else {
            return self.recv_head(io, i, 0);
        }
        Handled::Pending
    }

    /// `off` bytes of the request body have been sent.
    fn on_body_sent<IO: Io>(&mut self, io: &mut IO, i: usize, off: u32) -> Handled {
        if off < self.conns[i].body.map_or(0, |b| b.1) {
            self.send_body(io, i, off);
            Handled::Pending
        } else {
            self.recv_head(io, i, 0)
        }
    }

    /// `have` bytes of the response body are in the caller's buffer.
    fn on_body_bytes<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) -> Handled {
        if have == self.conns[i].body_len {
            self.succeed(io, i)
        } else {
            self.recv_body(io, i, have)
        }
    }

    /// `have` bytes of a discarded response body have been read.
    fn on_drained<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) -> Handled {
        if have == self.conns[i].body_len {
            self.succeed(io, i)
        } else {
            self.drain(io, i, have)
        }
    }
}
