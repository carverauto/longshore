// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The ListObjectsV2 response.

use super::*;

impl Server {
    /// Answer an S3 `ListObjectsV2` request from the object table.
    pub(super) fn list<IO: Io>(&mut self, io: &mut IO, i: usize) {
        let c = self.conns[i];
        let query = &c.target[2..c.target_len];
        // Percent-decode one query parameter into a fixed buffer.
        let param = |name: &[u8]| -> ([u8; TARGET_MAX], usize) {
            let mut out = ([0u8; TARGET_MAX], 0usize);
            for kv in query.split(|b| *b == b'&') {
                let Some(v) = kv.strip_prefix(name).and_then(|r| r.strip_prefix(b"=")) else {
                    continue;
                };
                out.1 = percent_decode(v, &mut out.0);
            }
            out
        };
        let (prefix, prefix_len) = param(b"prefix");
        let (after, after_len) = param(b"start-after");
        let (delimiter, delimiter_len) = param(b"delimiter");
        let delimiter = &delimiter[..delimiter_len];
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
                let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                resumed.0[resumed.1] = nibble(pair[0]) << 4 | nibble(pair[1]);
                resumed.1 += 1;
            }
        }
        let after = if resumed.1 > 0 {
            &resumed.0[..resumed.1]
        } else {
            &after[..after_len]
        };
        let prefix = &prefix[..prefix_len];
        if !prefix.starts_with(self.list_only_under.as_bytes()) {
            return self.error_document(io, i, 403, "AccessDenied");
        }

        // Emit matching keys in order, smallest first, without sorting the table.
        let cap = self.cfg.buffer_size as usize - 256;
        let mut n = 0usize;
        let mut emitted = 0usize;
        let mut last = ([0u8; TARGET_MAX], 0usize);
        last.0[..after.len()].copy_from_slice(after);
        last.1 = after.len();
        let mut truncated = false;
        {
            let body = io.buf(self.body_buf(i));
            fn put(body: &mut [u8], n: &mut usize, bytes: &[u8]) {
                body[*n..*n + bytes.len()].copy_from_slice(bytes);
                *n += bytes.len();
            }
            put(body, &mut n, b"<ListBucketResult>");
            loop {
                // The smallest key greater than the last one emitted.
                let mut best: Option<([u8; TARGET_MAX], usize, u32)> = None;
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
                        && k > &last.0[..last.1]
                        && best.as_ref().is_none_or(|b| k < &b.0[..b.1])
                    {
                        best = Some((key, len, o.len));
                    }
                }
                let Some((key, len, size)) = best else { break };
                if emitted == max_keys || n + len * 6 + 96 > cap {
                    truncated = true;
                    break;
                }
                fn put_escaped(body: &mut [u8], n: &mut usize, text: &[u8]) {
                    for b in text {
                        match b {
                            b'&' => put(body, n, b"&amp;"),
                            b'<' => put(body, n, b"&lt;"),
                            b'>' => put(body, n, b"&gt;"),
                            b'"' => put(body, n, b"&#34;"),
                            _ => put(body, n, &[*b]),
                        }
                    }
                }
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
                    put(body, &mut n, b"<CommonPrefixes><Prefix>");
                    put_escaped(body, &mut n, &key[..common]);
                    put(body, &mut n, b"</Prefix></CommonPrefixes>");
                    // No key is a byte string that continues with 0xFF, so
                    // this sorts after every key below the common prefix.
                    last = (key, common + 1);
                    last.0[common] = 0xFF;
                    emitted += 1;
                    continue;
                }
                put(body, &mut n, b"<Contents><Key>");
                put_escaped(body, &mut n, &key[..len]);
                put(body, &mut n, b"</Key><Size>");
                let mut digits = [0u8; 10];
                let mut d = digits.len();
                let mut v = size;
                loop {
                    d -= 1;
                    digits[d] = b'0' + (v % 10) as u8;
                    v /= 10;
                    if v == 0 {
                        break;
                    }
                }
                put(body, &mut n, &digits[d..]);
                put(body, &mut n, b"</Size></Contents>");
                last = (key, len);
                emitted += 1;
            }
            if truncated {
                put(body, &mut n, b"<NextContinuationToken>t");
                for b in &last.0[..last.1] {
                    put(body, &mut n, &[b"0123456789abcdef"[(b >> 4) as usize]]);
                    put(body, &mut n, &[b"0123456789abcdef"[(b & 15) as usize]]);
                }
                put(body, &mut n, b"</NextContinuationToken>");
                put(body, &mut n, b"<IsTruncated>true</IsTruncated>");
            } else {
                put(body, &mut n, b"<IsTruncated>false</IsTruncated>");
            }
            put(body, &mut n, b"</ListBucketResult>");
        }
        self.status(io, i, 200, "OK", n as u64, n as u32, false)
    }
}
