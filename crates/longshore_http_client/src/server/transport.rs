// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Accepting, reading, writing and closing connections, and writing response heads.

use super::*;

impl Server {
    pub(super) fn accept<IO: Io>(&mut self, io: &mut IO) {
        if self.accepting {
            return;
        }
        if let Some(i) = self.conns.iter().position(|c| c.state == State::Free) {
            self.conns[i].state = State::Accepting;
            self.accepting = true;
            io.submit(
                Token(self.accept_token()),
                Op::Accept {
                    listener: self.cfg.listener,
                    sock: self.sock(i),
                },
            )
            .expect("Limits::ops_max must cover Server::ops_in_flight_max");
        }
    }

    /// Receive through the connection's stream. With TLS the bytes may
    /// already be here, in which case the result is handled at once.
    pub(super) fn recv_into<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        buf: BufId,
        off: u32,
        len: u32,
    ) {
        match self.streams[i].recv(io, buf, off, len) {
            Step::Pending => {}
            Step::Done(r) => self.on_plain(io, i, K_RECV, r),
            Step::Tls(_) => self.close(io, i),
        }
    }

    pub(super) fn send_from<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        buf: BufId,
        off: u32,
        len: u32,
    ) {
        self.streams[i].send(io, buf, off, len, u32::MAX);
    }

    pub(super) fn close<IO: Io>(&mut self, io: &mut IO, i: usize) {
        self.streams[i].reset();
        self.conns[i].state = State::Closing;
        self.submit(io, i, K_CLOSE, Op::Close { sock: self.sock(i) });
    }

    pub(super) fn recv_head<IO: Io>(&mut self, io: &mut IO, i: usize, have: u32) {
        self.conns[i].state = State::RecvHead { have };
        self.recv_into(io, i, self.head_buf(i), have, self.cfg.head_max - have);
    }

    /// Send `len` bytes of the head buffer, then `body_len` of the body buffer.
    pub(super) fn respond<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        head_len: usize,
        body_len: u32,
        close_after: bool,
    ) {
        let c = &mut self.conns[i];
        c.head_len = head_len as u32;
        c.body_len = body_len;
        c.close_after |= close_after;
        c.state = State::SendHead { off: 0 };
        self.send_from(io, i, self.head_buf(i), 0, head_len as u32);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn status<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        status: u16,
        reason: &str,
        content_length: u64,
        body_len: u32,
        close: bool,
    ) {
        let limit = self.cfg.head_max as usize;
        let buf = &mut io.buf(self.head_buf(i))[..limit];
        let mut w = HeadWriter::response(buf, status, reason);
        w.header_u64("Content-Length", content_length);
        if close {
            w.header("Connection", b"close");
        }
        if let Some(sum) = self.conns[i].reply_checksum.take() {
            if sum[8..] == [0; 4] {
                let mut value = *b"crc32c=AAAAAAAA";
                value[7..].copy_from_slice(&sum[..8]);
                w.header("x-goog-hash", &value);
            } else {
                w.header("x-amz-checksum-crc64nvme", &sum);
            }
        }
        if let Some(slot) = self.conns[i].reply_object.take() {
            let o = &self.objects[slot as usize];
            w.header("ETag", &multipart::tag(&o.data[..o.len as usize]));
            w.header("Last-Modified", describing::LAST_MODIFIED);
            for (name, value) in describing::lines(&o.describing[..o.describing_len]) {
                w.header(name, value);
            }
        }
        if let Some(tag) = self.conns[i].reply_tag.take() {
            w.header("ETag", &tag);
        }
        let n = w.finish().expect("server heads are small and well-formed");
        self.respond(io, i, n, body_len, close);
    }
}
