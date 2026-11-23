// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Carrying out one request against the object table. Scripted misbehaviour
//! is in `scripted`.

use super::*;

impl Server {
    /// A complete request has been read; its body, if any, is in the body buffer.
    pub(super) fn handle<IO: Io>(&mut self, io: &mut IO, i: usize) {
        self.requests += 1;
        let behavior = self.next_behavior();
        if self.misbehave(io, i, behavior) {
            return;
        }
        let c = self.conns[i];
        if !c.authorized {
            let doc = b"<?xml version=\"1.0\"?><Error><Code>InvalidAccessKeyId</Code></Error>";
            return self.document(io, i, 403, "Forbidden", doc);
        }
        let target = &c.target[..c.target_len];
        let method = &c.method[..c.method_len];
        let apply_then_close = behavior == Behavior::ApplyThenClose;
        let is_listing = target.starts_with(b"/?")
            && target[2..]
                .split(|b| *b == b'&')
                .any(|pair| pair == b"list-type=2");
        if method == b"POST" && target == b"/?delete=" {
            return self.erase_many(io, i);
        }
        if method == b"PUT" && c.copy_source_len > 0 {
            return self.copy(io, i, apply_then_close);
        }
        if method == b"GET" && is_listing {
            return self.list(io, i);
        }
        if self.multipart(io, i, apply_then_close) {
            return;
        }
        let found = self.find(target);
        match method {
            b"PUT" => self.put(io, i, &c, found, apply_then_close),
            b"GET" | b"HEAD" => self.get(io, i, &c, found, behavior == Behavior::TruncateBody),
            b"DELETE" => self.delete(io, i, &c, found, apply_then_close),
            _ => self.status(io, i, 405, "Method Not Allowed", 0, 0, true),
        }
    }

    /// The slot of the stored object `target` names.
    pub(super) fn find(&self, target: &[u8]) -> Option<usize> {
        self.objects
            .iter()
            .position(|o| o.used && &o.target[..o.target_len] == target)
    }

    /// Respond with `status` and the document `doc` as the body.
    fn document<IO: Io>(&mut self, io: &mut IO, i: usize, status: u16, reason: &str, doc: &[u8]) {
        io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
        self.status(
            io,
            i,
            status,
            reason,
            doc.len() as u64,
            doc.len() as u32,
            false,
        );
    }

    /// Store the request body as the object `c` names, at `found` if it exists.
    fn put<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        c: &Conn,
        found: Option<usize>,
        apply_then_close: bool,
    ) {
        if found.is_some() && c.if_none_match {
            return self.status(io, i, 412, "Precondition Failed", 0, 0, false);
        }
        let Some(slot) = found.or_else(|| self.objects.iter().position(|o| !o.used)) else {
            return self.status(io, i, 507, "Insufficient Storage", 0, 0, false);
        };
        let len = c.request_len as usize;
        if self.refuse_checksum(io, i, c) {
            return;
        }
        if !self.sha256_agrees(&c.asked, &io.buf(self.body_buf(i))[..len]) {
            return self.error_document(io, i, 400, "BadDigest");
        }
        let kept = self.keep(&c.asked);
        let o = &mut self.objects[slot];
        o.kept = kept;
        o.checksum = c.claimed.filter(|_| self.cfg.checksum.is_some());
        o.data[..len].copy_from_slice(&io.buf(BufId(self.cfg.first_buf + i as u32 * 2 + 1))[..len]); // body buffer
        o.target[..c.target_len].copy_from_slice(&c.target[..c.target_len]);
        o.target_len = c.target_len;
        o.len = len as u32;
        o.used = true;
        o.describing = c.describing;
        o.describing_len = c.describing_len;
        if apply_then_close {
            return self.close(io, i);
        }
        // A stored part is answered with its tag, and so is an object.
        self.conns[i].reply_tag = Some(multipart::tag(&self.objects[slot].data[..len]));
        if !c.target[..c.target_len]
            .windows(12)
            .any(|w| w == b"?partNumber=")
        {
            self.conns[i].asked.reply = Some(slot as u32);
        }
        self.status(io, i, 200, "OK", 0, 0, false);
    }

    /// Refuse a body whose claimed checksum is wrong, the way a real store
    /// does. Returns whether it was refused.
    fn refuse_checksum<IO: Io>(&mut self, io: &mut IO, i: usize, c: &Conn) -> bool {
        let (Some(sum), Some(claimed)) = (self.cfg.checksum, c.claimed) else {
            return false;
        };
        if sum(&io.buf(self.body_buf(i))[..c.request_len as usize]) == claimed {
            return false;
        }
        // Each store names this error differently; answer as the one whose header was used.
        let doc: &[u8] = if claimed[8..] == [0; 4] {
            b"<?xml version='1.0' encoding='UTF-8'?><Error><Code>CrcMismatch</Code><Message>The CRC32C you specified did not match what we computed.</Message></Error>"
        } else {
            b"<?xml version=\"1.0\"?><Error><Code>BadDigest</Code><Message>checksum mismatch</Message></Error>"
        };
        self.document(io, i, 400, "Bad Request", doc);
        true
    }

    /// Serve the object at `found`, whole or the range asked for; HEAD
    /// sends no body, and `truncate` sends half of it.
    fn get<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        c: &Conn,
        found: Option<usize>,
        truncate: bool,
    ) {
        let Some(slot) = found else {
            return self.status(io, i, 404, "Not Found", 0, 0, false);
        };
        if let Some((status, reason)) = self.refused(&c.asked, slot) {
            return self.status(io, i, status, reason, 0, 0, false);
        }
        let is_get = &c.method[..c.method_len] == b"GET";
        self.conns[i].reply_object = Some(slot as u32);
        self.conns[i].asked.reply = Some(slot as u32);
        let o = &self.objects[slot];
        // A satisfiable range is served as 206 with just those bytes.
        if let Some((a, b)) = c.range.filter(|(a, b)| a <= b && *a < o.len && is_get) {
            let b = b.min(o.len - 1);
            let n = (b - a + 1) as usize;
            self.conns[i].asked.reply_range = Some((a, b));
            io.buf(self.body_buf(i))[..n].copy_from_slice(&o.data[a as usize..a as usize + n]);
            return self.status(io, i, 206, "Partial Content", n as u64, n as u32, false);
        }
        io.buf(self.body_buf(i))[..o.len as usize].copy_from_slice(&o.data[..o.len as usize]);
        // S3 returns a checksum only when asked; Google Cloud Storage always does.
        self.conns[i].reply_checksum = o
            .checksum
            .filter(|sum| c.checksum_mode || sum[8..] == [0; 4]);
        let len = o.len;
        let send = if !is_get {
            0
        } else if truncate {
            len / 2
        } else {
            len
        };
        self.status(io, i, 200, "OK", u64::from(len), send, truncate);
    }

    /// Delete the object at `found`.
    fn delete<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        c: &Conn,
        found: Option<usize>,
        apply_then_close: bool,
    ) {
        match found {
            Some(slot) => self.objects[slot].used = false,
            // Google Cloud Storage refuses to delete what is not there; S3 does not.
            None if c.bearer => {
                let doc = b"<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>";
                return self.document(io, i, 404, "Not Found", doc);
            }
            None => {}
        }
        if apply_then_close {
            return self.close(io, i);
        }
        self.status(io, i, 204, "No Content", 0, 0, false);
    }
}
