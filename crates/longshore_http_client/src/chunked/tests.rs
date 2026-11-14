// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Decode `raw` delivered in pieces of `step` bytes.
fn decode(raw: &[u8], step: usize) -> Result<(Vec<u8>, usize), HttpError> {
    let mut buf = vec![0u8; raw.len() + 16];
    let (mut d, mut decoded, mut end, mut fed) = (Chunked::new(), 0usize, 0usize, 0usize);
    while fed < raw.len() && !d.is_done() {
        let n = step.min(raw.len() - fed);
        buf[end..end + n].copy_from_slice(&raw[fed..fed + n]);
        fed += n;
        let used = d.feed(&mut buf, &mut decoded, end, end + n)?;
        if used < n {
            // Complete with bytes left over: report how many were unused.
            return Ok((buf[..decoded].to_vec(), raw.len() - (fed - n + used)));
        }
        // Everything given was consumed, so new bytes go right after the decoded body.
        end = decoded;
    }
    if !d.is_done() {
        return Err(HttpError::HeadTooLarge); // stands in for "incomplete" in these tests
    }
    Ok((buf[..decoded].to_vec(), 0))
}

const WIKI: &[u8] = b"4\r\nWiki\r\n7\r\npedia i\r\nB\r\nn \r\nchunks.\r\n0\r\n\r\n";

#[test]
fn decodes_the_classic_example_however_it_is_split() {
    for step in 1..=WIKI.len() {
        assert_eq!(
            decode(WIKI, step),
            Ok((b"Wikipedia in \r\nchunks.".to_vec(), 0)),
            "step {step}"
        );
    }
}

#[test]
fn handles_extensions_trailers_case_and_empty_bodies() {
    assert_eq!(decode(b"0\r\n\r\n", 1), Ok((vec![], 0)));
    assert_eq!(
        decode(b"5;name=value\r\nhello\r\n0\r\n\r\n", 3),
        Ok((b"hello".to_vec(), 0))
    );
    assert_eq!(
        decode(
            b"a\r\n0123456789\r\nA\r\nabcdefghij\r\n0\r\nX-Checksum: abc\r\nY: z\r\n\r\n",
            2
        ),
        Ok((b"0123456789abcdefghij".to_vec(), 0))
    );
    assert_eq!(
        decode(b"000000000003\r\nabc\r\n0\r\n\r\n", 5),
        Ok((b"abc".to_vec(), 0)),
        "leading zeros"
    );
}

#[test]
fn stops_at_the_end_of_the_body_and_reports_what_follows() {
    assert_eq!(
        decode(b"1\r\nx\r\n0\r\n\r\nHTTP/1.1 200", 100),
        Ok((b"x".to_vec(), 12))
    );
}

#[test]
fn rejects_malformed_framing() {
    for bad in [
        &b"\r\n"[..],
        b"g\r\n",
        b"5\nhello",
        b"5\r\nhelloXX",
        b"5\r\nhello\rX",
        b"3 \n",
        b"-1\r\n",
        b"11111111111111111\r\n",
        b"0\r\nbad\nheader\r\n\r\n",
        b"0\r\n\rX",
    ] {
        assert_eq!(
            decode(bad, 1).err(),
            Some(HttpError::BadFraming),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    // Incomplete input is not an error; it simply is not done.
    let mut d = Chunked::new();
    let mut buf = b"5\r\nhel".to_vec();
    let mut decoded = 0;
    assert_eq!(d.feed(&mut buf, &mut decoded, 0, 6), Ok(6));
    assert!(!d.is_done() && decoded == 3);
}

/// Whitespace after a chunk size may only be followed by an extension or
/// the end of the line. Anything else is not a size.
#[test]
fn whitespace_after_a_chunk_size_must_lead_to_an_extension_or_the_line_end() {
    let decode = |raw: &[u8]| {
        let mut d = Chunked::new();
        let mut buf = raw.to_vec();
        let mut decoded = 0;
        let n = buf.len();
        d.feed(&mut buf, &mut decoded, 0, n)
            .map(|_| (d.is_done(), buf[..decoded].to_vec()))
    };
    for ok in [
        &b"5 \r\nhello\r\n0\r\n\r\n"[..],
        b"5 ;name=value\r\nhello\r\n0\r\n\r\n",
        b"5\t \t\r\nhello\r\n0 \r\n\r\n",
    ] {
        assert_eq!(
            decode(ok),
            Ok((true, b"hello".to_vec())),
            "{:?}",
            String::from_utf8_lossy(ok)
        );
    }
    for bad in [
        &b"5 5\r\nhello\r\n0\r\n\r\n"[..],
        b"5 garbage\r\nhello\r\n0\r\n\r\n",
        b"5 \nhello\r\n0\r\n\r\n",
    ] {
        assert_eq!(
            decode(bad),
            Err(HttpError::BadFraming),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}
