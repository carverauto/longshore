// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart uploads in the test server.
//!
//! A part is kept in the object table under its whole request target, query
//! included, so it never answers to a plain key. Completion assembles the
//! parts the request lists into one object, which must fit one buffer.

use super::*;

/// Uploads in progress at most.
pub(super) const UPLOADS_MAX: usize = 8;
/// A part's tag: sixteen hex digits in quotes.
pub(super) const TAG_LEN: usize = 18;
/// Parts one completion may list.
const PARTS_MAX: usize = 32;
const ID_PREFIX: &[u8] = b"sim-upload-";

/// The tag of a part with these bytes: its length and a hash of its content.
pub(super) fn tag(part: &[u8]) -> [u8; TAG_LEN] {
    let mut hash = 0x811c_9dc5u32;
    for b in part {
        hash = (hash ^ *b as u32).wrapping_mul(0x0100_0193);
    }
    let digits = format_hex((part.len() << 32) | hash as usize).0;
    let mut out = [b'"'; TAG_LEN];
    out[1..17].copy_from_slice(&digits);
    out
}

/// The decimal number at the start of `text`.
fn number(text: &[u8]) -> Option<u32> {
    let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    core::str::from_utf8(&text[..digits]).ok()?.parse().ok()
}

/// What follows `name` in `text`, if it occurs.
fn after<'a>(text: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let at = text.windows(name.len()).position(|w| w == name)?;
    Some(&text[at + name.len()..])
}

impl Server {
    pub(super) fn error_document<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        status: u16,
        code: &str,
    ) {
        let body = io.buf(self.body_buf(i));
        let mut n = 0;
        for piece in [
            &b"<?xml version=\"1.0\"?><Error><Code>"[..],
            code.as_bytes(),
            b"</Code></Error>",
        ] {
            body[n..n + piece.len()].copy_from_slice(piece);
            n += piece.len();
        }
        self.status(io, i, status, "Error", n as u64, n as u32, false)
    }

    /// Handle the request if it is a multipart one other than storing a
    /// part, which the ordinary PUT path does once the upload is known.
    /// Returns whether the request was answered.
    pub(super) fn multipart<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        apply_then_close: bool,
    ) -> bool {
        let c = self.conns[i];
        let target = &c.target[..c.target_len];
        let method = &c.method[..c.method_len];
        let Some(split) = target.iter().position(|b| *b == b'?') else {
            return false;
        };
        let (path, query) = (&target[..split], &target[split + 1..]);
        if method == b"POST" && query == b"uploads=" {
            let Some(entry) = self.uploads.iter().position(|id| *id == 0) else {
                {
                    self.status(io, i, 507, "Insufficient Storage", 0, 0, false);
                    return true;
                }
            };
            self.uploads_begun += 1;
            self.uploads[entry] = self.uploads_begun;
            if apply_then_close {
                {
                    self.close(io, i);
                    return true;
                }
            }
            let id = format_hex(self.uploads_begun as usize);
            let body = io.buf(self.body_buf(i));
            let mut n = 0;
            for piece in [
                &b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<InitiateMultipartUploadResult><Bucket>bucket</Bucket><UploadId>"[..],
                ID_PREFIX,
                &id.0[16 - id.1..],
                b"</UploadId></InitiateMultipartUploadResult>",
            ] {
                body[n..n + piece.len()].copy_from_slice(piece);
                n += piece.len();
            }
            {
                self.status(io, i, 200, "OK", n as u64, n as u32, false);
                return true;
            }
        }
        let Some(id_text) = after(query, b"uploadId=") else {
            return false;
        };
        let id = id_text
            .strip_prefix(ID_PREFIX)
            .and_then(|hex| core::str::from_utf8(hex).ok())
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .filter(|id| *id != 0);
        let entry = id.and_then(|id| self.uploads.iter().position(|u| *u == id));
        let Some(entry) = entry else {
            {
                self.error_document(io, i, 404, "NoSuchUpload");
                return true;
            }
        };
        // Parts of this upload are the objects whose target ends like this.
        let is_part = |o: &Object| {
            let t = &o.target[..o.target_len];
            o.used
                && t.starts_with(path)
                && t[path.len()..].starts_with(b"?partNumber=")
                && after(t, b"uploadId=") == Some(id_text)
        };
        match method {
            b"PUT" => false,
            b"DELETE" => {
                for o in self.objects.iter_mut() {
                    if is_part(o) {
                        o.used = false;
                    }
                }
                self.uploads[entry] = 0;
                if apply_then_close {
                    {
                        self.close(io, i);
                        return true;
                    }
                }
                self.status(io, i, 204, "No Content", 0, 0, false);
                true
            }
            b"POST" => {
                // Read the list of parts, then reuse the body buffer to assemble them.
                let mut listed = [(0u32, [0u8; TAG_LEN]); PARTS_MAX];
                let mut count = 0;
                {
                    let mut rest = &io.buf(self.body_buf(i))[..c.request_len as usize];
                    while let Some(at) = after(rest, b"<PartNumber>") {
                        let (Some(n), Some(tag_at)) = (number(at), after(at, b"<ETag>&quot;"))
                        else {
                            {
                                self.error_document(io, i, 400, "MalformedXML");
                                return true;
                            }
                        };
                        if count == PARTS_MAX || tag_at.len() < 16 {
                            {
                                self.error_document(io, i, 400, "MalformedXML");
                                return true;
                            }
                        }
                        let mut t = [b'"'; TAG_LEN];
                        t[1..17].copy_from_slice(&tag_at[..16]);
                        listed[count] = (n, t);
                        count += 1;
                        rest = tag_at;
                    }
                }
                let mut total = 0usize;
                for (n, claimed_tag) in &listed[..count] {
                    let part = self.objects.iter().find(|o| {
                        is_part(o)
                            && after(&o.target[..o.target_len], b"?partNumber=").and_then(number)
                                == Some(*n)
                    });
                    let Some(part) =
                        part.filter(|p| tag(&p.data[..p.len as usize]) == *claimed_tag)
                    else {
                        {
                            self.error_document(io, i, 400, "InvalidPart");
                            return true;
                        }
                    };
                    let len = part.len as usize;
                    if total + len > self.cfg.buffer_size as usize {
                        {
                            self.error_document(io, i, 400, "EntityTooLarge");
                            return true;
                        }
                    }
                    io.buf(BufId(self.cfg.first_buf + i as u32 * 2 + 1))[total..total + len]
                        .copy_from_slice(&part.data[..len]);
                    total += len;
                }
                if count == 0 {
                    {
                        self.error_document(io, i, 400, "MalformedXML");
                        return true;
                    }
                }
                if let (Some(sum), Some(claimed)) = (self.cfg.checksum, c.claimed) {
                    if sum(&io.buf(self.body_buf(i))[..total]) != claimed {
                        {
                            self.error_document(io, i, 400, "BadDigest");
                            return true;
                        }
                    }
                }
                for o in self.objects.iter_mut() {
                    if is_part(o) {
                        o.used = false;
                    }
                }
                self.uploads[entry] = 0;
                let slot = self
                    .objects
                    .iter()
                    .position(|o| o.used && &o.target[..o.target_len] == path)
                    .or_else(|| self.objects.iter().position(|o| !o.used))
                    .expect("the parts just freed leave room");
                let o = &mut self.objects[slot];
                o.data[..total].copy_from_slice(
                    &io.buf(BufId(self.cfg.first_buf + i as u32 * 2 + 1))[..total],
                );
                o.target[..path.len()].copy_from_slice(path);
                o.target_len = path.len();
                o.len = total as u32;
                o.used = true;
                o.checksum = c.claimed.filter(|_| self.cfg.checksum.is_some());
                if apply_then_close {
                    {
                        self.close(io, i);
                        return true;
                    }
                }
                let doc = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CompleteMultipartUploadResult><Key>key</Key><ETag>&quot;assembled-1&quot;</ETag></CompleteMultipartUploadResult>";
                io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
                self.status(io, i, 200, "OK", doc.len() as u64, doc.len() as u32, false);
                true
            }
            _ => false,
        }
    }
}
