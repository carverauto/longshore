// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart uploads in the test server.
//!
//! A part is kept in the object table under its whole request target, query
//! included, so it never answers to a plain key. Completion assembles the
//! parts the request lists into one object, which must fit one buffer. A
//! part stored with a CRC-64/NVME must be listed with that checksum, as
//! stricter stores require of an upload that keeps checksums.

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
        hash = (hash ^ u32::from(*b)).wrapping_mul(0x0100_0193);
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
            self.begin_upload(io, i, apply_then_close);
            return true;
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
            self.error_document(io, i, 404, "NoSuchUpload");
            return true;
        };
        let upload = Upload {
            path,
            id_text,
            entry,
        };
        match method {
            b"PUT" => false,
            b"DELETE" => {
                self.abort_upload(io, i, &upload, apply_then_close);
                true
            }
            b"POST" => {
                self.complete_upload(io, i, &c, &upload, apply_then_close);
                true
            }
            _ => false,
        }
    }

    /// Begin an upload and answer with its id.
    fn begin_upload<IO: Io>(&mut self, io: &mut IO, i: usize, apply_then_close: bool) {
        let Some(entry) = self.uploads.iter().position(|id| *id == 0) else {
            return self.status(io, i, 507, "Insufficient Storage", 0, 0, false);
        };
        self.uploads_begun += 1;
        self.uploads[entry] = self.uploads_begun;
        if apply_then_close {
            return self.close(io, i);
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
        self.status(io, i, 200, "OK", n as u64, n as u32, false);
    }

    /// Forget an upload and its parts.
    fn abort_upload<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        upload: &Upload<'_>,
        apply_then_close: bool,
    ) {
        self.end_upload(upload);
        if apply_then_close {
            return self.close(io, i);
        }
        self.status(io, i, 204, "No Content", 0, 0, false);
    }

    /// Free an upload's parts and its entry.
    fn end_upload(&mut self, upload: &Upload<'_>) {
        for o in self.objects.iter_mut() {
            if upload.has_part(o) {
                o.used = false;
            }
        }
        self.uploads[upload.entry] = 0;
    }

    /// Assemble the parts the request lists into one object.
    fn complete_upload<IO: Io>(
        &mut self,
        io: &mut IO,
        i: usize,
        c: &Conn,
        upload: &Upload<'_>,
        apply_then_close: bool,
    ) {
        // Read the list of parts, then reuse the body buffer to assemble them.
        let parsed = listed_parts(&io.buf(self.body_buf(i))[..c.request_len as usize]);
        let (listed, count) = match parsed {
            Ok(parts) => parts,
            Err(code) => return self.error_document(io, i, 400, code),
        };
        let total = match self.assemble(io, i, upload, &listed[..count]) {
            Ok(total) => total,
            Err(code) => return self.error_document(io, i, 400, code),
        };
        if count == 0 {
            return self.error_document(io, i, 400, "MalformedXML");
        }
        if let (Some(sum), Some(claimed)) = (self.cfg.checksum(), c.claimed) {
            if sum(&io.buf(self.body_buf(i))[..total]) != claimed {
                return self.error_document(io, i, 400, "BadDigest");
            }
        }
        self.end_upload(upload);
        self.store_assembled(io, i, upload.path, total);
        if apply_then_close {
            return self.close(io, i);
        }
        let doc = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CompleteMultipartUploadResult><Key>key</Key><ETag>&quot;assembled-1&quot;</ETag></CompleteMultipartUploadResult>";
        io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
        self.status(io, i, 200, "OK", doc.len() as u64, doc.len() as u32, false);
    }

    /// Copy the `listed` parts, in order, into the body buffer. Returns
    /// their total length, or the error code a part is refused with.
    fn assemble<IO: Io>(
        &self,
        io: &mut IO,
        i: usize,
        upload: &Upload<'_>,
        listed: &[Listed],
    ) -> Result<usize, &'static str> {
        let mut total = 0usize;
        for (n, claimed_tag, claimed_sum) in listed {
            let part = self.objects.iter().find(|o| {
                upload.has_part(o)
                    && after(&o.target[..o.target_len], b"?partNumber=").and_then(number)
                        == Some(*n)
            });
            let part = part
                .filter(|p| tag(&p.data[..p.len as usize]) == *claimed_tag)
                .ok_or("InvalidPart")?;
            // A CRC-32C (four zero bytes after it) is never listed.
            let kept = part.checksum.filter(|sum| sum[8..] != [0; 4]);
            if kept.is_some() && claimed_sum.is_none() {
                return Err("InvalidRequest");
            }
            if claimed_sum.is_some() && *claimed_sum != kept {
                return Err("InvalidPart");
            }
            let len = part.len as usize;
            if total + len > self.cfg.buffer_size() as usize {
                return Err("EntityTooLarge");
            }
            io.buf(self.body_buf(i))[total..total + len].copy_from_slice(&part.data[..len]);
            total += len;
        }
        Ok(total)
    }

    /// Store the `total` assembled bytes in the body buffer as the object `path`.
    fn store_assembled<IO: Io>(&mut self, io: &mut IO, i: usize, path: &[u8], total: usize) {
        #[expect(
            clippy::expect_used,
            reason = "the object's parts were just freed, so a slot is free"
        )]
        let slot = self
            .find(path)
            .or_else(|| self.objects.iter().position(|o| !o.used))
            .expect("the parts just freed leave room");
        let o = &mut self.objects[slot];
        o.data[..total]
            .copy_from_slice(&io.buf(BufId(self.cfg.first_buf() + i as u32 * 2 + 1))[..total]);
        o.target[..path.len()].copy_from_slice(path);
        o.target_len = path.len();
        o.len = total as u32;
        o.used = true;
        // A store that keeps checksums has one for the assembled object
        // whether or not the request claimed it.
        o.checksum = self.cfg.checksum().map(|sum| sum(&o.data[..total]));
    }
}

/// A part a completion lists: its number, its tag and its CRC-64/NVME, if listed.
type Listed = (u32, [u8; TAG_LEN], Option<[u8; 12]>);

/// The upload a request names.
struct Upload<'a> {
    /// The object it assembles.
    path: &'a [u8],
    /// Its id as the request wrote it.
    id_text: &'a [u8],
    /// Its entry in the uploads in progress.
    entry: usize,
}

impl Upload<'_> {
    /// Parts of this upload are the objects whose target ends like this.
    fn has_part(&self, o: &Object) -> bool {
        let t = &o.target[..o.target_len];
        o.used
            && t.starts_with(self.path)
            && t[self.path.len()..].starts_with(b"?partNumber=")
            && after(t, b"uploadId=") == Some(self.id_text)
    }
}

/// The parts a completion request's body lists, and how many there are, or
/// the error code the list is refused with.
fn listed_parts(body: &[u8]) -> Result<([Listed; PARTS_MAX], usize), &'static str> {
    let mut listed = [(0u32, [0u8; TAG_LEN], None); PARTS_MAX];
    let mut count = 0;
    let mut rest = body;
    while let Some(at) = after(rest, b"<PartNumber>") {
        let (Some(n), Some(tag_at)) = (number(at), after(at, b"<ETag>&quot;")) else {
            return Err("MalformedXML");
        };
        if count == PARTS_MAX || tag_at.len() < 16 {
            return Err("MalformedXML");
        }
        let mut t = [b'"'; TAG_LEN];
        t[1..17].copy_from_slice(&tag_at[..16]);
        let end = after(tag_at, b"</Part>").map_or(0, |e| tag_at.len() - e.len());
        let sum: Option<[u8; 12]> = after(&tag_at[..end], b"<ChecksumCRC64NVME>")
            .and_then(|v| v.get(..12)?.try_into().ok());
        listed[count] = (n, t, sum);
        count += 1;
        rest = tag_at;
    }
    Ok((listed, count))
}
