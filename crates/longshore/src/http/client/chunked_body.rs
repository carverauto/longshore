// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving a chunked body, including the framing that follows a full buffer.

use super::*;

impl Client {
    /// The buffer a chunked body is decoded in, and its usable size: the
    /// caller's response buffer, or the room after the head when the caller
    /// gave none (the body is then kept only if it fits there).
    pub(super) fn chunked_area(&self, i: usize) -> (BufId, u32) {
        match self.conns[i].response_buf {
            Some(buf) => (buf, self.cfg.buffer_size),
            None => (self.head_buf(i), self.cfg.head_max),
        }
    }

    /// Begin decoding a chunked body. `have - head_len` raw bytes arrived
    /// with the head.
    pub(super) fn start_chunked<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        head_len: u32,
        have: u32,
    ) -> Handled {
        let extra = have - head_len;
        let base = match self.conns[i].response_buf {
            Some(dst) => {
                if extra > self.cfg.buffer_size {
                    return self.fail(io, i, Error::BodyTooLarge);
                }
                // Move the raw bytes that came with the head into the body buffer.
                let (head, body) = io.buf_pair(self.head_buf(i), dst);
                body[..extra as usize].copy_from_slice(&head[head_len as usize..have as usize]);
                0
            }
            // They are already where the body will be decoded.
            None => head_len,
        };
        self.conns[i].chunked = Chunked::new();
        self.conns[i].body_base = base;
        self.on_chunked_bytes(io, i, base, base + extra)
    }

    /// Raw bytes `decoded..raw_end` of the body buffer have arrived.
    pub(super) fn on_chunked_bytes<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        decoded: u32,
        raw_end: u32,
    ) -> Handled {
        let (buf, cap) = self.chunked_area(i);
        let mut at = decoded as usize;
        let mut decoder = self.conns[i].chunked;
        let used = match decoder.feed(io.buf(buf), &mut at, decoded as usize, raw_end as usize) {
            Ok(n) => n,
            Err(e) => return self.fail(io, i, Error::Head(e)),
        };
        self.conns[i].chunked = decoder;
        if decoder.is_done() {
            if used < (raw_end - decoded) as usize {
                // Bytes after the end of the body: the framing cannot be trusted.
                return self.fail(io, i, Error::Head(HttpError::BadFraming));
            }
            self.conns[i].body_len = at as u32 - self.conns[i].body_base;
            return self.succeed(io, i);
        }
        // Everything received was consumed; more goes right after the decoded bytes.
        if at as u32 == cap {
            // Exactly full. A body of this size is allowed, but its closing
            // framing has not been read and there is no room for it here.
            // With a caller's buffer, read it into the space after the head;
            // a body being discarded has only the head buffer, and is too large.
            if self.conns[i].response_buf.is_none() {
                return self.fail(io, i, Error::BodyTooLarge);
            }
            self.conns[i].body_len = at as u32 - self.conns[i].body_base;
            return self.recv_chunked_tail(io, i);
        }
        self.conns[i].state = State::RecvChunked { decoded: at as u32 };
        let step = self.streams[i].recv(io, buf, at as u32, cap - at as u32);
        self.after_recv(io, i, step)
    }

    /// Where the end of a chunked body's framing is read once the caller's
    /// buffer is full: the head buffer, after the head it still holds.
    pub(super) fn chunked_tail_area(&self, i: usize) -> (u32, u32) {
        let head_len = self.conns[i].fields.map_or(0, |f| f.head_len as u32);
        (head_len, self.cfg.head_max - head_len)
    }

    pub(super) fn recv_chunked_tail<IO: Io>(&mut self, io: &mut IO, i: usize) -> Handled {
        let (at, room) = self.chunked_tail_area(i);
        if room == 0 {
            return self.fail(io, i, Error::BodyTooLarge);
        }
        self.conns[i].state = State::RecvChunkedTail;
        let step = self.streams[i].recv(io, self.head_buf(i), at, room);
        self.after_recv(io, i, step)
    }

    /// `n` more bytes of framing arrived after a full buffer.
    pub(super) fn on_chunked_tail<IO: Io>(&mut self, io: &mut IO, i: usize, n: u32) -> Handled {
        let (start, _) = self.chunked_tail_area(i);
        let mut decoder = self.conns[i].chunked;
        let mut payload_end = start as usize;
        let used = match decoder.feed(
            io.buf(self.head_buf(i)),
            &mut payload_end,
            start as usize,
            (start + n) as usize,
        ) {
            Ok(used) => used,
            Err(e) => return self.fail(io, i, Error::Head(e)),
        };
        // Any payload here is a body longer than the buffer that was full.
        if payload_end > start as usize {
            return self.fail(io, i, Error::BodyTooLarge);
        }
        self.conns[i].chunked = decoder;
        if decoder.is_done() {
            if used < n as usize {
                return self.fail(io, i, Error::Head(HttpError::BadFraming));
            }
            return self.succeed(io, i);
        }
        self.recv_chunked_tail(io, i)
    }
}
