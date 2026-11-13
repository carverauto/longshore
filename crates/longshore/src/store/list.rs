// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Parser for S3 `ListObjectsV2` responses.
//!
//! A pull parser over the response bytes: it allocates nothing and copies
//! nothing. It understands exactly the structure it needs (`Contents`
//! elements with `Key` and `Size`, and the top-level `IsTruncated`) and
//! skips everything else, so extra fields from different stores are harmless.

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ListError {
    /// The document is not a `ListBucketResult`.
    NotAListing,
    /// An element is unterminated, or a required field is missing or invalid.
    Malformed,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Entry<'a> {
    /// The key as it appears in the XML, possibly with character references
    /// (`&amp;`, `&#34;`). Use [`unescape`] to decode it.
    pub key_raw: &'a [u8],
    pub size: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Listing<'a> {
    body: &'a [u8],
    /// More keys exist after the last one returned.
    pub is_truncated: bool,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Content of the first `<name>...</name>` directly in `xml`, and the offset
/// just past its closing tag.
fn element<'a>(xml: &'a [u8], name: &str) -> Result<Option<(&'a [u8], usize)>, ListError> {
    let (mut open, mut close) = ([0u8; 40], [0u8; 40]);
    let n = name.len();
    open[0] = b'<';
    open[1..1 + n].copy_from_slice(name.as_bytes());
    open[1 + n] = b'>';
    close[..2].copy_from_slice(b"</");
    close[2..2 + n].copy_from_slice(name.as_bytes());
    close[2 + n] = b'>';
    let Some(start) = find(xml, &open[..n + 2]) else {
        return Ok(None);
    };
    let content = start + n + 2;
    let len = find(&xml[content..], &close[..n + 3]).ok_or(ListError::Malformed)?;
    Ok(Some((&xml[content..content + len], content + len + n + 3)))
}

impl<'a> Listing<'a> {
    pub fn parse(xml: &'a [u8]) -> Result<Listing<'a>, ListError> {
        // The root element, possibly after an XML declaration, possibly with
        // a namespace attribute.
        let root = find(xml, b"<ListBucketResult").ok_or(ListError::NotAListing)?;
        let after = *xml.get(root + 17).ok_or(ListError::Malformed)?;
        if !matches!(after, b'>' | b' ' | b'\t' | b'\r' | b'\n') {
            return Err(ListError::NotAListing);
        }
        let open_end = root + find(&xml[root..], b">").ok_or(ListError::Malformed)? + 1;
        let end = find(&xml[open_end..], b"</ListBucketResult>").ok_or(ListError::Malformed)?;
        let body = &xml[open_end..open_end + end];
        // IsTruncated is a child of the root; look for it outside Contents.
        let mut is_truncated = None;
        let mut rest = body;
        loop {
            let next_contents = find(rest, b"<Contents>");
            let scope = &rest[..next_contents.unwrap_or(rest.len())];
            if let Some((v, _)) = element(scope, "IsTruncated")? {
                is_truncated = Some(match v {
                    b"true" => true,
                    b"false" => false,
                    _ => return Err(ListError::Malformed),
                });
            }
            let Some(at) = next_contents else { break };
            let close = find(&rest[at..], b"</Contents>").ok_or(ListError::Malformed)?;
            rest = &rest[at + close + 11..];
        }
        Ok(Listing {
            body,
            is_truncated: is_truncated.ok_or(ListError::Malformed)?,
        })
    }

    pub fn entries(&self) -> Entries<'a> {
        Entries {
            rest: self.body,
            failed: false,
        }
    }
}

pub struct Entries<'a> {
    rest: &'a [u8],
    failed: bool,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<Entry<'a>, ListError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let (contents, past) = match element(self.rest, "Contents") {
            Ok(Some(v)) => v,
            Ok(None) => return None,
            Err(e) => {
                self.failed = true;
                return Some(Err(e));
            }
        };
        self.rest = &self.rest[past..];
        let field =
            |name| element(contents, name).and_then(|v| v.map(|v| v.0).ok_or(ListError::Malformed));
        let entry = field("Key").and_then(|key_raw| {
            let size = field("Size")?;
            if key_raw.is_empty()
                || size.is_empty()
                || size.len() > 20
                || !size.iter().all(u8::is_ascii_digit)
            {
                return Err(ListError::Malformed);
            }
            let size = size
                .iter()
                .try_fold(0u64, |n, d| {
                    n.checked_mul(10)?.checked_add((d - b'0') as u64)
                })
                .ok_or(ListError::Malformed)?;
            Ok(Entry { key_raw, size })
        });
        self.failed = entry.is_err();
        Some(entry)
    }
}

/// Decode XML character references in `raw` into `out`. Returns the decoded
/// length, or `None` if a reference is invalid or `out` is too small. The
/// result is never longer than the input.
pub fn unescape(raw: &[u8], out: &mut [u8]) -> Option<usize> {
    let (mut i, mut n) = (0, 0);
    while i < raw.len() {
        if raw[i] != b'&' {
            *out.get_mut(n)? = raw[i];
            (i, n) = (i + 1, n + 1);
            continue;
        }
        let end = i + raw[i..].iter().position(|b| *b == b';')?;
        let name = &raw[i + 1..end];
        let ch: u32 = match name {
            b"amp" => '&' as u32,
            b"lt" => '<' as u32,
            b"gt" => '>' as u32,
            b"quot" => '"' as u32,
            b"apos" => '\'' as u32,
            [b'#', b'x', hex @ ..]
                if !hex.is_empty() && hex.len() <= 6 && hex.iter().all(u8::is_ascii_hexdigit) =>
            {
                u32::from_str_radix(core::str::from_utf8(hex).ok()?, 16).ok()?
            }
            [b'#', dec @ ..]
                if !dec.is_empty() && dec.len() <= 7 && dec.iter().all(u8::is_ascii_digit) =>
            {
                core::str::from_utf8(dec).ok()?.parse().ok()?
            }
            _ => return None,
        };
        let c = char::from_u32(ch)?;
        let dst = out.get_mut(n..n + c.len_utf8())?;
        c.encode_utf8(dst);
        n += c.len_utf8();
        i = end + 1;
    }
    Some(n)
}

#[cfg(test)]
mod tests;
