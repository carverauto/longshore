// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! XML character references, for the keys in a request body the server
//! reads. The store's own decoder is in `longshore_s3::list`, which this
//! crate cannot depend on.

/// Decode XML character references in `raw` into `out`. Returns the decoded
/// length, or `None` if a reference is invalid or `out` is too small. The
/// result is never longer than the input.
pub(super) fn unescape(raw: &[u8], out: &mut [u8]) -> Option<usize> {
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
