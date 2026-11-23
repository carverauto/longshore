// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scripted misbehaviour: answers a test queued in place of the store's own.

use super::*;

impl Server {
    /// The behavior queued for the next request, `Normal` if none is.
    pub(super) fn next_behavior(&mut self) -> Behavior {
        if self.script_len == 0 {
            return Behavior::Normal;
        }
        let b = self.script[self.script_head];
        self.script_head = (self.script_head + 1) % SCRIPT_MAX;
        self.script_len -= 1;
        b
    }

    /// Answer the request on connection `i` as `behavior` scripts, if it
    /// replaces the store's answer. Returns whether it did.
    pub(super) fn misbehave<IO: Io>(&mut self, io: &mut IO, i: usize, behavior: Behavior) -> bool {
        match behavior {
            Behavior::CloseBeforeResponse => self.close(io, i),
            Behavior::TlsAlert(description) => {
                // Sent like a two-byte head; the connection closes after it.
                let conn = &mut self.conns[i];
                (conn.head_len, conn.body_len, conn.close_after) = (2, 0, true);
                conn.state = State::SendHead { off: 0 };
                let buf = self.head_buf(i);
                self.streams[i].send_alert(io, buf, 0, 2, description);
            }
            Behavior::Stall => {
                // Wait for the client to give up; a read notices it closing.
                self.conns[i].state = State::Stalled;
                self.recv_into(io, i, self.head_buf(i), 0, 1);
            }
            Behavior::GarbageHead => {
                self.raw(io, i, b"\x00\x01 this is not HTTP \xff\xfe\r\n\r\n");
            }
            Behavior::EndlessHead => {
                let n = self.cfg.head_max as usize;
                let buf = &mut io.buf(self.head_buf(i))[..n];
                buf.fill(b'a');
                buf[..22].copy_from_slice(b"HTTP/1.1 200 OK\r\nX-A: ");
                self.respond(io, i, n, 0, true);
            }
            Behavior::Raw(bytes) => self.raw(io, i, bytes),
            Behavior::Status(s) => self.status(io, i, s, "Scripted", 0, 0, false),
            Behavior::StatusWithBody(s, n) => {
                io.buf(self.body_buf(i))[..n as usize].fill(b'e');
                self.status(io, i, s, "Scripted", u64::from(n), n, false);
            }
            Behavior::ErrorDocument(s, code) => self.error_document(io, i, s, code),
            // The server closes after this response, and says so.
            Behavior::Chunked => self.raw(
                io,
                i,
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n0\r\n\r\n",
            ),
            Behavior::BadChunk => self.raw(
                io,
                i,
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\noops\r\n0\r\n\r\n",
            ),
            Behavior::ChunkedBody(chunk) => self.chunked_body(io, i, chunk),
            Behavior::Normal | Behavior::TruncateBody | Behavior::ApplyThenClose => return false,
        }
        true
    }

    /// Send exactly `bytes` as the whole response, then close.
    fn raw<IO: Io>(&mut self, io: &mut IO, i: usize, bytes: &[u8]) {
        io.buf(self.head_buf(i))[..bytes.len()].copy_from_slice(bytes);
        self.respond(io, i, bytes.len(), 0, true);
    }

    /// Serve the object the request names with chunked transfer encoding,
    /// in chunks of `chunk` bytes.
    fn chunked_body<IO: Io>(&mut self, io: &mut IO, i: usize, chunk: u32) {
        let c = &self.conns[i];
        let Some(slot) = self.find(&c.target[..c.target_len]) else {
            return self.status(io, i, 404, "Not Found", 0, 0, false);
        };
        // Encode the object into the body buffer, chunk by chunk.
        let o = &self.objects[slot];
        let n = encode_chunked(&o.data[..o.len as usize], chunk, io.buf(self.body_buf(i)));
        let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        io.buf(self.head_buf(i))[..head.len()].copy_from_slice(head);
        self.respond(io, i, head.len(), n as u32, false);
    }
}

/// Write `data` into `body` as chunks of `chunk` bytes and the last chunk.
/// Returns the encoded length.
fn encode_chunked(data: &[u8], chunk: u32, body: &mut [u8]) -> usize {
    let (mut n, mut sent) = (0usize, 0usize);
    while sent < data.len() {
        let k = (chunk.max(1) as usize).min(data.len() - sent);
        // A size line, the data, its line end, and the last chunk.
        assert!(
            n + 18 + k + 2 + 5 <= body.len(),
            "ChunkedBody({chunk}): the encoded object does not fit the \
             server's buffer; use larger chunks or a smaller object"
        );
        let (digits, len) = format_hex(k);
        body[n..n + len].copy_from_slice(&digits[16 - len..]);
        n += len;
        body[n..n + 2].copy_from_slice(b"\r\n");
        body[n + 2..n + 2 + k].copy_from_slice(&data[sent..sent + k]);
        body[n + 2 + k..n + 4 + k].copy_from_slice(b"\r\n");
        n += 4 + k;
        sent += k;
    }
    body[n..n + 5].copy_from_slice(b"0\r\n\r\n");
    n + 5
}
