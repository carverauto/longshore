// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests for the HTTP head codec (Hegel).

use hegel::generators as gs;
use hegel::TestCase;
use longshore::http::head::{parse_request, parse_response, HeadWriter, HttpError};

const LIMIT: usize = 2048;
const TOKEN: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.!~";
const VISIBLE: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.~/%?=&+:;,()'\"*@$!";

/// A string of 1..=max characters drawn from `alphabet`.
fn string_from(tc: &TestCase, alphabet: &[u8], max: usize) -> String {
    let picks = tc.draw(
        gs::vecs(
            gs::integers::<usize>()
                .min_value(0)
                .max_value(alphabet.len() - 1),
        )
        .min_size(1)
        .max_size(max),
    );
    picks.into_iter().map(|i| alphabet[i] as char).collect()
}

/// Arbitrary bytes never panic, and a parsed head never points outside its input.
#[hegel::test]
fn parsing_arbitrary_bytes_is_safe(tc: TestCase) {
    let data = tc.draw(gs::binary().max_size(600));
    if let Ok(Some(h)) = parse_response(&data, LIMIT) {
        assert!(h.fields.head_len <= data.len());
        assert!(data[..h.fields.head_len].ends_with(b"\r\n\r\n"));
        let _ = h.fields.header(&data, "content-length");
    }
    if let Ok(Some(h)) = parse_request(&data, LIMIT) {
        assert!(h.fields.head_len <= data.len());
        let _ = (h.method(&data), h.target(&data));
    }
}

/// A verdict on a prefix is final: once a prefix parses or is rejected,
/// receiving more bytes never changes the answer. This is what makes the
/// result independent of how the network splits the head.
#[hegel::test]
fn a_verdict_on_a_prefix_never_changes(tc: TestCase) {
    // Mix structured heads with raw bytes so both valid and invalid inputs are common.
    let mut data = Vec::new();
    if tc.draw(gs::booleans()) {
        let status = tc.draw(gs::integers::<u16>().min_value(100).max_value(599));
        data.extend_from_slice(
            format!("HTTP/1.1 {status} {}\r\n", string_from(&tc, TOKEN, 8)).as_bytes(),
        );
        for _ in 0..tc.draw(gs::integers::<usize>().min_value(0).max_value(6)) {
            let name = tc.draw(gs::sampled_from(vec![
                "Content-Length",
                "Connection",
                "ETag",
                "Transfer-Encoding",
                "x-amz-id",
            ]));
            let value = tc.draw(gs::sampled_from(vec![
                "0",
                "17",
                "close",
                "keep-alive",
                "chunked",
                "\"abc\"",
                " padded ",
                "1x",
            ]));
            data.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(&tc.draw(gs::binary().max_size(64)));
    let limit = tc.draw(gs::integers::<usize>().min_value(16).max_value(LIMIT));
    let cut = tc.draw(gs::integers::<usize>().min_value(0).max_value(data.len()));

    let (prefix, whole) = (
        parse_response(&data[..cut], limit),
        parse_response(&data, limit),
    );
    match prefix {
        Ok(None) => {}
        decided => assert_eq!(
            decided,
            whole,
            "prefix of {cut} bytes decided differently from all {} bytes",
            data.len()
        ),
    }
}

/// Whatever the writer accepts, the parser reads back unchanged.
// This property found a real bug: a 20-digit Content-Length (>= 10^19) was
// written but not parsed. The unit tests in head.rs pin that case.
#[hegel::test]
fn written_heads_parse_back(tc: TestCase) {
    let method = tc.draw(gs::sampled_from(vec![
        "GET", "HEAD", "PUT", "DELETE", "POST",
    ]));
    let target = format!("/{}", string_from(&tc, VISIBLE, 40));
    let headers: Vec<(String, String)> = (0..tc
        .draw(gs::integers::<usize>().min_value(0).max_value(8)))
        .map(|i| {
            (
                format!("x-{i}-{}", string_from(&tc, TOKEN, 12)),
                string_from(&tc, VISIBLE, 30),
            )
        })
        .collect();
    let length = tc.draw(gs::integers::<u64>());

    let mut buf = [0u8; LIMIT];
    let mut w = HeadWriter::request(&mut buf, method, &target);
    w.header_u64("Content-Length", length);
    for (n, v) in &headers {
        w.header(n, v.as_bytes());
    }
    let n = w.finish().expect("valid fields that fit must be accepted");

    let head = parse_request(&buf[..n], LIMIT)
        .expect("writer output must parse")
        .expect("writer output is a complete head");
    assert_eq!(head.fields.head_len, n);
    assert_eq!(
        (head.method(&buf), head.target(&buf)),
        (method.as_bytes(), target.as_bytes())
    );
    assert_eq!(head.fields.content_length, Some(length));
    for (name, value) in &headers {
        assert_eq!(
            head.fields.header(&buf, &name.to_uppercase()),
            Some(value.as_bytes()),
            "header {name}"
        );
    }
}

/// Hostile field values cannot smuggle extra lines: the writer either refuses
/// them or emits exactly one line per field.
#[hegel::test]
fn writer_never_emits_injected_lines(tc: TestCase) {
    let target = tc.draw(gs::text().max_size(30));
    let name = tc.draw(gs::text().max_size(12));
    let value = tc.draw(gs::binary().max_size(40));
    let mut buf = [0u8; 512];
    let mut w = HeadWriter::request(&mut buf, "GET", &target);
    w.header(&name, &value);
    match w.finish() {
        Err(e) => assert_eq!(e, HttpError::BadRequest),
        Ok(n) => {
            let out = &buf[..n];
            assert_eq!(
                out.windows(2).filter(|w| *w == b"\r\n").count(),
                3,
                "request line, one header, blank line"
            );
            assert!(!out.contains(&0), "no NUL bytes");
            let head = parse_request(out, 512)
                .expect("accepted output must parse")
                .expect("complete");
            assert_eq!(head.target(out), target.as_bytes());
        }
    }
}
