// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The documents of a batch erase: the list of keys sent, and the failures
//! that come back.

use crate::store::list::ListError;

/// Keys one request may name.
pub const KEYS_MAX: usize = 1000;

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The text between `open` and `close` at the start of what follows `open`
/// in `xml`, and the offset just past `close`.
fn between<'a>(xml: &'a [u8], open: &[u8], close: &[u8]) -> Option<(&'a [u8], usize)> {
    let start = find(xml, open)? + open.len();
    let len = find(&xml[start..], close)?;
    Some((&xml[start..start + len], start + len + close.len()))
}

/// Write the request document naming `keys` into `out` and return its
/// length. The store is asked to report only the keys it could not erase.
/// `None` if there are no keys, more than [`KEYS_MAX`], an empty key, or
/// `out` is too small.
pub fn write_manifest(out: &mut [u8], keys: &[&str]) -> Option<usize> {
    if keys.is_empty() || keys.len() > KEYS_MAX || keys.iter().any(|k| k.is_empty()) {
        return None;
    }
    let mut n = 0;
    let mut put = |bytes: &[u8]| -> Option<()> {
        out.get_mut(n..n + bytes.len())?.copy_from_slice(bytes);
        n += bytes.len();
        Some(())
    };
    put(b"<Delete><Quiet>true</Quiet>")?;
    for key in keys {
        put(b"<Object><Key>")?;
        for b in key.as_bytes() {
            match b {
                b'&' => put(b"&amp;")?,
                b'<' => put(b"&lt;")?,
                b'>' => put(b"&gt;")?,
                b'"' => put(b"&quot;")?,
                // A carriage return would be normalised away by an XML parser.
                b'\r' => put(b"&#13;")?,
                _ => put(core::slice::from_ref(b))?,
            }
        }
        put(b"</Key></Object>")?;
    }
    put(b"</Delete>")?;
    Some(n)
}

/// One key the store did not erase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure<'a> {
    /// The key as it appears in the XML, possibly with character references.
    /// Decode it with [`crate::store::list::unescape`].
    pub key_raw: &'a [u8],
    /// The store's error code, such as `AccessDenied`.
    pub code: &'a [u8],
}

/// The response to a batch erase.
#[derive(Debug, Clone, Copy)]
pub struct Failures<'a> {
    rest: &'a [u8],
}

impl<'a> Failures<'a> {
    /// Read the response document. Iterating yields the keys that were not
    /// erased; none means every key named is gone.
    pub fn parse(xml: &'a [u8]) -> Result<Failures<'a>, ListError> {
        let root = find(xml, b"<DeleteResult").ok_or(ListError::NotAListing)?;
        let after = *xml.get(root + 13).ok_or(ListError::Malformed)?;
        // An empty result may be written as a self-closing element.
        if after == b'/' {
            return Ok(Failures { rest: &[] });
        }
        if !matches!(after, b'>' | b' ' | b'\t' | b'\r' | b'\n') {
            return Err(ListError::NotAListing);
        }
        let open_end = root + find(&xml[root..], b">").ok_or(ListError::Malformed)? + 1;
        if xml[open_end - 2] == b'/' {
            return Ok(Failures { rest: &[] });
        }
        let end = find(&xml[open_end..], b"</DeleteResult>").ok_or(ListError::Malformed)?;
        Ok(Failures {
            rest: &xml[open_end..open_end + end],
        })
    }
}

impl<'a> Iterator for Failures<'a> {
    type Item = Result<Failure<'a>, ListError>;

    fn next(&mut self) -> Option<Self::Item> {
        find(self.rest, b"<Error>")?;
        let Some((error, past)) = between(self.rest, b"<Error>", b"</Error>") else {
            self.rest = &[];
            return Some(Err(ListError::Malformed));
        };
        self.rest = &self.rest[past..];
        let key = between(error, b"<Key>", b"</Key>").map(|v| v.0);
        let code = between(error, b"<Code>", b"</Code>").map(|v| v.0);
        Some(match (key, code) {
            (Some(key_raw), Some(code)) if !key_raw.is_empty() => Ok(Failure { key_raw, code }),
            _ => {
                self.rest = &[];
                Err(ListError::Malformed)
            }
        })
    }
}

#[cfg(test)]
mod tests;
