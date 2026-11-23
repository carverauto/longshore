// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The ListObjectsV2 response.

use super::*;

/// A key, or another query value, in a fixed buffer, and its length.
type Key = ([u8; TARGET_MAX], usize);

/// What a listing request asks for.
struct Query {
    prefix: Key,
    /// List keys after this one: the continuation token's, or `start-after`.
    after: Key,
    delimiter: Key,
    max_keys: usize,
}

impl Server {
    /// Answer an S3 `ListObjectsV2` request from the object table.
    pub(super) fn list<IO: Io>(&mut self, io: &mut IO, i: usize) {
        let c = &self.conns[i];
        let q = read_query(&c.target[2..c.target_len]);
        if !q.prefix.0[..q.prefix.1].starts_with(self.list_only_under.as_bytes()) {
            return self.error_document(io, i, 403, "AccessDenied");
        }
        let n = self.write_listing(io.buf(self.body_buf(i)), &q);
        self.status(io, i, 200, "OK", n as u64, n as u32, false);
    }

    /// Write the listing `q` asks for into `body`; returns its length.
    fn write_listing(&self, body: &mut [u8], q: &Query) -> usize {
        let (prefix, delimiter) = (&q.prefix.0[..q.prefix.1], &q.delimiter.0[..q.delimiter.1]);
        let cap = self.cfg.buffer_size() as usize - 256;
        let mut out = Out { body, n: 0 };
        let (mut last, mut emitted, mut truncated) = (q.after, 0usize, false);
        out.put(b"<ListBucketResult>");
        // Emit matching keys in order, smallest first, without sorting the table.
        while let Some((key, size)) = self.next_key(prefix, &last.0[..last.1]) {
            if emitted == q.max_keys || out.n + key.1 * 6 + 96 > cap {
                truncated = true;
                break;
            }
            last = out.entry(&key, size, prefix, delimiter);
            emitted += 1;
        }
        if truncated {
            out.put(b"<NextContinuationToken>t");
            for b in &last.0[..last.1] {
                out.put(&[b"0123456789abcdef"[(b >> 4) as usize]]);
                out.put(&[b"0123456789abcdef"[(b & 15) as usize]]);
            }
            out.put(b"</NextContinuationToken>");
            out.put(b"<IsTruncated>true</IsTruncated>");
        } else {
            out.put(b"<IsTruncated>false</IsTruncated>");
        }
        out.put(b"</ListBucketResult>");
        out.n
    }

    /// The smallest stored key under `prefix` greater than `last`, and its size.
    fn next_key(&self, prefix: &[u8], last: &[u8]) -> Option<(Key, u32)> {
        let mut best: Option<(Key, u32)> = None;
        // Parts of uploads in progress are not objects yet.
        for o in self
            .objects
            .iter()
            .filter(|o| o.used && !o.target[..o.target_len].contains(&b'?'))
        {
            let mut key = [0u8; TARGET_MAX];
            let len = percent_decode(&o.target[1..o.target_len], &mut key);
            let k = &key[..len];
            if k.starts_with(prefix)
                && k > last
                && best.as_ref().is_none_or(|b| k < &b.0 .0[..b.0 .1])
            {
                best = Some(((key, len), o.len));
            }
        }
        best
    }
}

/// Read a listing's query string.
fn read_query(query: &[u8]) -> Query {
    // Percent-decode one query parameter into a fixed buffer.
    let param = |name: &[u8]| -> Key {
        let mut out = ([0u8; TARGET_MAX], 0usize);
        for kv in query.split(|b| *b == b'&') {
            let Some(v) = kv.strip_prefix(name).and_then(|r| r.strip_prefix(b"=")) else {
                continue;
            };
            out.1 = percent_decode(v, &mut out.0);
        }
        out
    };
    let (max_raw, max_len) = param(b"max-keys");
    let max_keys = core::str::from_utf8(&max_raw[..max_len])
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000);
    // A continuation token is "t" and the hex of where the last page
    // stopped: opaque to the client, and not a key it could pass back
    // as start-after.
    let token = query
        .split(|b| *b == b'&')
        .rev()
        .find_map(|kv| kv.strip_prefix(b"continuation-token="))
        .unwrap_or(b"");
    let mut resumed = ([0u8; TARGET_MAX], 0usize);
    if let Some(hex) = token.strip_prefix(b"t") {
        for pair in hex.as_chunks::<2>().0.iter().take(TARGET_MAX) {
            let nibble = |digit: u8| (digit as char).to_digit(16).unwrap_or(0) as u8;
            resumed.0[resumed.1] = nibble(pair[0]) << 4 | nibble(pair[1]);
            resumed.1 += 1;
        }
    }
    Query {
        prefix: param(b"prefix"),
        after: if resumed.1 > 0 {
            resumed
        } else {
            param(b"start-after")
        },
        delimiter: param(b"delimiter"),
        max_keys,
    }
}

/// A listing body being written.
struct Out<'a> {
    body: &'a mut [u8],
    n: usize,
}

impl Out<'_> {
    fn put(&mut self, bytes: &[u8]) {
        self.body[self.n..self.n + bytes.len()].copy_from_slice(bytes);
        self.n += bytes.len();
    }

    fn put_escaped(&mut self, text: &[u8]) {
        for b in text {
            match b {
                b'&' => self.put(b"&amp;"),
                b'<' => self.put(b"&lt;"),
                b'>' => self.put(b"&gt;"),
                b'"' => self.put(b"&#34;"),
                _ => self.put(&[*b]),
            }
        }
    }

    fn put_decimal(&mut self, mut v: u32) {
        let mut digits = [0u8; 10];
        let mut d = digits.len();
        loop {
            d -= 1;
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.put(&digits[d..]);
    }

    /// Write the entry for `key`, of `size` bytes. Returns the key the
    /// listing goes on after.
    fn entry(&mut self, key: &Key, size: u32, prefix: &[u8], delimiter: &[u8]) -> Key {
        let (key, len) = *key;
        // A key with the delimiter after the prefix is reported once,
        // as the prefix that ends at the delimiter, for all such keys.
        let rolled = (!delimiter.is_empty())
            .then(|| {
                key[prefix.len()..len]
                    .windows(delimiter.len())
                    .position(|w| w == delimiter)
            })
            .flatten()
            .map(|at| prefix.len() + at + delimiter.len());
        if let Some(common) = rolled.filter(|common| *common < TARGET_MAX) {
            self.put(b"<CommonPrefixes><Prefix>");
            self.put_escaped(&key[..common]);
            self.put(b"</Prefix></CommonPrefixes>");
            // No key is a byte string that continues with 0xFF, so
            // this sorts after every key below the common prefix.
            let mut last = (key, common + 1);
            last.0[common] = 0xFF;
            return last;
        }
        self.put(b"<Contents><Key>");
        self.put_escaped(&key[..len]);
        self.put(b"</Key><Size>");
        self.put_decimal(size);
        self.put(b"</Size></Contents>");
        (key, len)
    }
}
