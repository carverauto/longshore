// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The chunked decoder: property tests (Hegel) and the edges its unit tests
//! leave out.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_http_client::chunked::Chunked;
use longshore_http_client::head::HttpError;

/// What a decoder made of some raw bytes.
#[derive(Debug, PartialEq, Eq)]
struct Decoded {
    body: Vec<u8>,
    done: bool,
    /// Raw bytes it consumed.
    used: usize,
}

/// Feed `raw` in pieces of the sizes in `steps` (repeated as needed), the
/// way the client does: each piece is placed right after the body decoded
/// so far. Checks the decoder's own promises on every call.
fn decode(raw: &[u8], steps: &[usize]) -> Result<Decoded, HttpError> {
    let mut buf = vec![0u8; raw.len() + 16];
    let (mut d, mut decoded, mut fed) = (Chunked::new(), 0usize, 0usize);
    let mut step = steps.iter().cycle();
    while fed < raw.len() && !d.is_done() {
        let n = (*step.next().unwrap()).clamp(1, raw.len() - fed);
        let start = decoded;
        buf[start..start + n].copy_from_slice(&raw[fed..fed + n]);
        let used = d.feed(&mut buf, &mut decoded, start, start + n)?;
        assert!(used <= n, "consumed more than it was given");
        assert!(used == n || d.is_done(), "stopped early without being done");
        assert!(decoded <= start + used, "decoded past the bytes it read");
        fed += used;
        if used < n {
            break;
        }
    }
    buf.truncate(decoded);
    Ok(Decoded {
        body: buf,
        done: d.is_done(),
        used: fed,
    })
}

fn sizes(tc: &TestCase, max: usize, most: usize) -> Vec<usize> {
    tc.draw(
        gs::vecs(gs::integers::<usize>().min_value(1).max_value(max))
            .min_size(1)
            .max_size(most),
    )
}

/// Any body, cut into any chunks, with or without extensions, trailers and
/// bytes after the end, decodes to itself however the network splits it.
#[hegel::test]
fn an_encoded_body_decodes_to_itself(tc: TestCase) {
    let body = tc.draw(gs::binary().max_size(2000));
    let chunks = sizes(&tc, 700, 8);
    let mut raw = Vec::new();
    let (mut at, mut chunk) = (0, chunks.iter().cycle());
    while at < body.len() {
        let n = (*chunk.next().unwrap()).min(body.len() - at);
        let size = match tc.draw(gs::integers::<u8>().min_value(0).max_value(2)) {
            0 => format!("{n:x}"),
            1 => format!("{n:X}"),
            _ => format!("{n:08x}"),
        };
        raw.extend_from_slice(size.as_bytes());
        if tc.draw(gs::booleans()) {
            raw.extend_from_slice(b";name=value");
        }
        raw.extend_from_slice(b"\r\n");
        raw.extend_from_slice(&body[at..at + n]);
        raw.extend_from_slice(b"\r\n");
        at += n;
    }
    raw.extend_from_slice(b"0\r\n");
    for _ in 0..tc.draw(gs::integers::<usize>().min_value(0).max_value(3)) {
        raw.extend_from_slice(b"X-Trailer: value\r\n");
    }
    raw.extend_from_slice(b"\r\n");
    let encoded = raw.len();
    raw.extend_from_slice(&tc.draw(gs::binary().max_size(40)));

    let steps = sizes(&tc, 300, 6);
    assert_eq!(
        decode(&raw, &steps),
        Ok(Decoded {
            body,
            done: true,
            used: encoded
        })
    );
}

/// Arbitrary bytes never panic, and the verdict does not depend on how they
/// were split: the same error, or the same body, state and bytes consumed.
#[hegel::test]
fn the_verdict_does_not_depend_on_the_split(tc: TestCase) {
    // Start some inputs like a chunked body so the later states are reached.
    let mut raw = Vec::new();
    if tc.draw(gs::booleans()) {
        let n = tc.draw(gs::integers::<usize>().min_value(0).max_value(20));
        raw.extend_from_slice(format!("{n:x}\r\n").as_bytes());
    }
    raw.extend_from_slice(&tc.draw(gs::binary().max_size(300)));
    let whole = decode(&raw, &[raw.len().max(1)]);
    let split = decode(&raw, &sizes(&tc, 9, 6));
    assert_eq!(whole, split);
    assert_eq!(whole, decode(&raw, &[1]));
}

#[test]
fn sixteen_hex_digits_are_a_size_and_seventeen_are_not() {
    assert_eq!(
        decode(b"0000000000000001\r\nx\r\n0\r\n\r\n", &[1]),
        Ok(Decoded {
            body: b"x".to_vec(),
            done: true,
            used: 26
        })
    );
    // The largest size there is: accepted, and simply not finished.
    let r = decode(b"ffffffffffffffff\r\nabc", &[4]).unwrap();
    assert_eq!((r.body, r.done), (b"abc".to_vec(), false));
    assert_eq!(
        decode(b"00000000000000001\r\nx\r\n0\r\n\r\n", &[1]),
        Err(HttpError::BadFraming)
    );
}

#[test]
fn a_size_line_must_end_with_cr_lf() {
    for bad in [&b"5;ext\nhello"[..], b"5\rXhello", b"5;ext\r\rhello"] {
        for step in [1, 3, 64] {
            assert_eq!(
                decode(bad, &[step]),
                Err(HttpError::BadFraming),
                "{:?} in steps of {step}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}

/// The malformed inputs of the unit tests were tried one byte at a time
/// only. Whole, and in threes, they are refused the same.
#[test]
fn malformed_framing_is_refused_however_it_arrives() {
    for bad in [
        &b"\r\n"[..],
        b"g\r\n",
        b"5\nhello",
        b"5\r\nhelloXX",
        b"5\r\nhello\rX",
        b"-1\r\n",
        b"0\r\nbad\nheader\r\n\r\n",
        b"0\r\n\rX",
        b"5 5\r\nhello\r\n0\r\n\r\n",
    ] {
        for step in [1, 2, 3, 64] {
            assert_eq!(
                decode(bad, &[step]),
                Err(HttpError::BadFraming),
                "{:?} in steps of {step}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
