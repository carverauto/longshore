// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Values and documents of a multipart upload.
//!
//! The caller holds an upload's state: the [`UploadId`] returned when it
//! begins and one [`PartTag`] per part stored, with the checksum the part
//! was sent with. The store keeps no table of open uploads. To finish, the
//! caller writes the list of parts into a registered buffer with
//! [`write_manifest`] and sends it with `S3::multipart_complete`.

/// Longest upload id a store may return.
pub const UPLOAD_ID_MAX: usize = 256;
/// Longest part tag (the `ETag` of a stored part, quotes included).
pub const PART_TAG_MAX: usize = 64;
/// Part numbers run from 1 to this.
pub const PART_NUMBER_MAX: u32 = 10_000;

/// Fixed-capacity bytes: the two identifier types below.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Fixed<const N: usize> {
    bytes: [u8; N],
    len: u16,
}

impl<const N: usize> Fixed<N> {
    /// `None` if `bytes` is empty or longer than the type holds. Nothing is cut.
    pub fn new(bytes: &[u8]) -> Option<Fixed<N>> {
        if bytes.is_empty() || bytes.len() > N {
            return None;
        }
        let mut out = [0u8; N];
        out[..bytes.len()].copy_from_slice(bytes);
        Some(Fixed {
            bytes: out,
            len: bytes.len() as u16,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl<const N: usize> core::fmt::Debug for Fixed<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(self.as_bytes()))
    }
}

/// Names one multipart upload in progress.
pub type UploadId = Fixed<UPLOAD_ID_MAX>;
/// What the store returned for one stored part. Completion sends it back.
pub type PartTag = Fixed<PART_TAG_MAX>;

/// One part as the completion document lists it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Part {
    /// 1 to [`PART_NUMBER_MAX`].
    pub number: u32,
    pub tag: PartTag,
    /// The part's CRC-64/NVME in base64, as it was sent with the part
    /// (`crate::checksum::crc64_header_value`). A store that keeps a
    /// checksum for the upload refuses a completion that leaves it out of
    /// any part. `None` for a part sent without one, and always on Google
    /// Cloud Storage, whose part checksums are CRC-32C.
    pub checksum: Option<[u8; 12]>,
}

/// The text of the first `<name>...</name>` element in `doc`.
fn element<'a>(doc: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let name = name.as_bytes();
    let mut at = 0;
    while let Some(open) = doc[at..].iter().position(|b| *b == b'<') {
        let start = at + open + 1;
        let rest = &doc[start..];
        if rest.starts_with(name) && rest.get(name.len()) == Some(&b'>') {
            let text = &rest[name.len() + 1..];
            let end = text.iter().position(|b| *b == b'<')?;
            let close = &text[end..];
            let ok = close.starts_with(b"</")
                && close[2..].starts_with(name)
                && close.get(2 + name.len()) == Some(&b'>');
            return ok.then_some(&text[..end]);
        }
        at = start;
    }
    None
}

/// The upload id in the response to beginning an upload.
pub fn upload_id_in(doc: &[u8]) -> Option<&[u8]> {
    element(doc, "UploadId").filter(|id| !id.is_empty())
}

/// The error code of an error document, if `doc` is one. Stores answer some
/// requests with status 200 and an error document, so a success status alone
/// does not mean success.
pub fn error_code_in(doc: &[u8]) -> Option<&[u8]> {
    let at = doc.windows(6).position(|w| w == b"<Error")?;
    Some(element(&doc[at..], "Code").unwrap_or(b""))
}

/// Write the completion document for `parts` into `out` and return its
/// length. Parts must be in ascending part-number order, which is the order
/// the store requires. `None` if they are not, a number is out of range, or
/// `out` is too small.
pub fn write_manifest(out: &mut [u8], parts: &[Part]) -> Option<usize> {
    let mut n = 0;
    let mut put = |bytes: &[u8]| -> Option<()> {
        out.get_mut(n..n + bytes.len())?.copy_from_slice(bytes);
        n += bytes.len();
        Some(())
    };
    put(b"<CompleteMultipartUpload>")?;
    let mut previous = 0;
    for part in parts {
        if part.number <= previous || part.number > PART_NUMBER_MAX {
            return None;
        }
        previous = part.number;
        let mut digits = [0u8; 5];
        let mut d = digits.len();
        let mut v = part.number;
        while v > 0 {
            d -= 1;
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        put(b"<Part><PartNumber>")?;
        put(&digits[d..])?;
        put(b"</PartNumber><ETag>")?;
        // A tag is quoted; the quotes are written as entities.
        for b in part.tag.as_bytes() {
            match b {
                b'"' => put(b"&quot;")?,
                b'&' => put(b"&amp;")?,
                b'<' => put(b"&lt;")?,
                b'>' => put(b"&gt;")?,
                _ => put(core::slice::from_ref(b))?,
            }
        }
        put(b"</ETag>")?;
        if let Some(sum) = &part.checksum {
            put(b"<ChecksumCRC64NVME>")?;
            put(sum)?;
            put(b"</ChecksumCRC64NVME>")?;
        }
        put(b"</Part>")?;
    }
    put(b"</CompleteMultipartUpload>")?;
    Some(n)
}

#[cfg(test)]
mod tests;
