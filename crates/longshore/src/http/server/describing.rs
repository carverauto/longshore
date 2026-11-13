// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the test server keeps about an object besides its bytes, and the
//! requests that work on objects without carrying one: copy and batch erase.

use super::*;

/// Every object was last modified at the same moment.
pub(super) const LAST_MODIFIED: &[u8] = b"Wed, 21 Oct 2015 07:28:00 GMT";

/// The `name: value` lines kept in a describing block.
pub(super) fn lines(block: &[u8]) -> impl Iterator<Item = (&str, &[u8])> {
    block.split(|b| *b == b'\n').filter_map(|line| {
        let colon = line.iter().position(|b| *b == b':')?;
        let name = core::str::from_utf8(&line[..colon]).ok()?;
        Some((name, line[colon + 1..].trim_ascii()))
    })
}

impl Conn {
    /// Keep the headers of a request head that describe the object it stores.
    pub(super) fn note_describing(&mut self, head: &[u8]) {
        self.describing_len = 0;
        for line in head.split(|b| *b == b'\n').skip(1) {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Some(colon) = line.iter().position(|b| *b == b':') else {
                continue;
            };
            let name = &line[..colon];
            let starts = |prefix: &[u8]| {
                name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix)
            };
            let kept = name.eq_ignore_ascii_case(b"content-type")
                || starts(b"x-amz-meta-")
                || starts(b"x-goog-meta-")
                || name.eq_ignore_ascii_case(b"x-amz-storage-class")
                || name.eq_ignore_ascii_case(b"x-goog-storage-class");
            let end = self.describing_len + line.len() + 1;
            if kept && end <= DESCRIBING_MAX {
                self.describing[self.describing_len..end - 1].copy_from_slice(line);
                self.describing[end - 1] = b'\n';
                self.describing_len = end;
            }
        }
    }
}

impl Server {
    /// Copy the object named by the request's copy source to its target.
    pub(super) fn copy<IO: Io>(&mut self, io: &mut IO, i: usize, apply_then_close: bool) {
        let c = self.conns[i];
        let source = &c.copy_source[..c.copy_source_len];
        // "/bucket/key": the object table knows the key as "/key".
        let key_at = source
            .iter()
            .skip(1)
            .position(|b| *b == b'/')
            .map_or(source.len(), |at| at + 1);
        let from = self
            .objects
            .iter()
            .position(|o| o.used && o.target[..o.target_len] == source[key_at..]);
        let Some(from) = from else {
            return self.error_document(io, i, 404, "NoSuchKey");
        };
        let to = self
            .objects
            .iter()
            .position(|o| o.used && o.target[..o.target_len] == c.target[..c.target_len])
            .or_else(|| self.objects.iter().position(|o| !o.used));
        let Some(to) = to else {
            return self.status(io, i, 507, "Insufficient Storage", 0, 0, false);
        };
        if to != from {
            let (len, checksum) = (self.objects[from].len, self.objects[from].checksum);
            let (describing, describing_len) = (
                self.objects[from].describing,
                self.objects[from].describing_len,
            );
            // Through the connection's body buffer: two objects cannot be
            // borrowed at once.
            let scratch = BufId(self.cfg.first_buf + i as u32 * 2 + 1);
            io.buf(scratch)[..len as usize]
                .copy_from_slice(&self.objects[from].data[..len as usize]);
            let o = &mut self.objects[to];
            o.data[..len as usize].copy_from_slice(&io.buf(scratch)[..len as usize]);
            o.target[..c.target_len].copy_from_slice(&c.target[..c.target_len]);
            o.target_len = c.target_len;
            (o.len, o.checksum, o.used) = (len, checksum, true);
            (o.describing, o.describing_len) = (describing, describing_len);
        }
        if apply_then_close {
            return self.close(io, i);
        }
        let doc = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CopyObjectResult><ETag>&quot;copied&quot;</ETag></CopyObjectResult>";
        io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
        self.status(io, i, 200, "OK", doc.len() as u64, doc.len() as u32, false)
    }

    /// Erase the keys the request body names and report the ones refused.
    pub(super) fn erase_many<IO: Io>(&mut self, io: &mut IO, i: usize) {
        let c = self.conns[i];
        let len = c.request_len as usize;
        if let (Some(sum), Some(claimed)) = (self.cfg.checksum, c.claimed) {
            if sum(&io.buf(self.body_buf(i))[..len]) != claimed {
                return self.error_document(io, i, 400, "BadDigest");
            }
        }
        // Refused keys are set aside while the request is read, because the
        // report is written over it. Eight are enough for the tests.
        let denied = self.erase_denied.as_bytes();
        let mut refused = [([0u8; TARGET_MAX], 0usize); 8];
        let mut refused_count = 0;
        {
            let body = &io.buf(self.body_buf(i))[..len];
            let mut rest = body;
            while let Some(at) = rest.windows(5).position(|w| w == b"<Key>") {
                let text = &rest[at + 5..];
                let Some(end) = text.windows(6).position(|w| w == b"</Key>") else {
                    return self.error_document(io, i, 400, "MalformedXML");
                };
                let mut key = [0u8; TARGET_MAX];
                let Some(key_len) = crate::store::list::unescape(&text[..end], &mut key) else {
                    return self.error_document(io, i, 400, "MalformedXML");
                };
                let key = &key[..key_len];
                if !denied.is_empty() && key.starts_with(denied) {
                    if refused_count < refused.len() {
                        refused[refused_count].0[..end].copy_from_slice(&text[..end]);
                        refused[refused_count].1 = end;
                        refused_count += 1;
                    }
                } else {
                    for o in self.objects.iter_mut().filter(|o| o.used) {
                        let mut decoded = [0u8; TARGET_MAX];
                        let n = percent_decode(&o.target[1..o.target_len], &mut decoded);
                        if &decoded[..n] == key {
                            o.used = false;
                        }
                    }
                }
                rest = &text[end + 6..];
            }
        }
        let body = io.buf(self.body_buf(i));
        let mut n = 0;
        let mut put = |bytes: &[u8]| {
            body[n..n + bytes.len()].copy_from_slice(bytes);
            n += bytes.len();
        };
        put(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DeleteResult>");
        for (key, key_len) in &refused[..refused_count] {
            put(b"<Error><Key>");
            put(&key[..*key_len]);
            put(b"</Key><Code>AccessDenied</Code><Message>Access Denied</Message></Error>");
        }
        put(b"</DeleteResult>");
        self.status(io, i, 200, "OK", n as u64, n as u32, false)
    }
}
