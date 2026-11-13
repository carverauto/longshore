// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

const LIMIT: usize = 4096;
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nETag: \"abc\"\r\nx-amz-request-id:  R1  \r\nConnection: keep-alive\r\n\r\nhello, world";

fn parse(b: &[u8]) -> Result<Option<ResponseHead>, HttpError> {
    parse_response(b, LIMIT)
}

#[test]
fn parses_a_response_head() {
    let h = parse(OK).unwrap().unwrap();
    assert_eq!(
        (
            h.status,
            h.fields.content_length,
            h.fields.chunked,
            h.fields.close
        ),
        (200, Some(12), false, false)
    );
    assert_eq!(&OK[h.fields.head_len..], b"hello, world");
    assert_eq!(h.fields.header(OK, "etag"), Some(&b"\"abc\""[..]));
    assert_eq!(
        h.fields.header(OK, "X-Amz-Request-Id"),
        Some(&b"R1"[..]),
        "value is trimmed"
    );
    assert_eq!(h.fields.header(OK, "missing"), None);
}

#[test]
fn result_does_not_depend_on_how_bytes_arrive() {
    let whole = parse(OK).unwrap();
    for cut in 0..OK.len() {
        match parse(&OK[..cut]).unwrap() {
            None => assert!(
                cut < whole.unwrap().fields.head_len,
                "incomplete at {cut} although the head was complete"
            ),
            Some(h) => assert_eq!(Some(h), whole, "prefix of {cut} bytes parsed differently"),
        }
    }
}

#[test]
fn connection_semantics() {
    let close = |s: &str| parse(s.as_bytes()).unwrap().unwrap().fields.close;
    assert!(!close("HTTP/1.1 200 OK\r\n\r\n"));
    assert!(close("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"));
    assert!(close(
        "HTTP/1.1 200 OK\r\nConnection: Keep-Alive, Close\r\n\r\n"
    ));
    assert!(
        close("HTTP/1.0 200 OK\r\n\r\n"),
        "HTTP/1.0 closes by default"
    );
    assert!(!close("HTTP/1.0 200 OK\r\nConnection: keep-alive\r\n\r\n"));
    assert!(close(
        "HTTP/1.0 200 OK\r\nConnection: keep-alive, close\r\n\r\n"
    ));
    assert!(close(
        "HTTP/1.0 200 OK\r\nConnection: keep-alive\r\nConnection: close\r\n\r\n"
    ));
}

#[test]
fn status_line_forms() {
    assert_eq!(
        parse(b"HTTP/1.1 503 Slow Down\r\n\r\n")
            .unwrap()
            .unwrap()
            .status,
        503
    );
    assert_eq!(
        parse(b"HTTP/1.1 204\r\n\r\n").unwrap().unwrap().status,
        204,
        "reason phrase is optional"
    );
    for bad in [
        "HTTP/2 200 OK\r\n\r\n",
        "HTTP/1.1 20 OK\r\n\r\n",
        "HTTP/1.1 2000 OK\r\n\r\n",
        "HTTP/1.1  200 OK\r\n\r\n",
        "http/1.1 200 OK\r\n\r\n",
        "HTTP/1.1 099 x\r\n\r\n",
        "ICY 200 OK\r\n\r\n",
    ] {
        assert_eq!(
            parse(bad.as_bytes()),
            Err(HttpError::BadStartLine),
            "{bad:?}"
        );
    }
    assert_eq!(
        parse(b"GARBAGE"),
        Err(HttpError::BadStartLine),
        "rejected before the head completes"
    );
    // Found by the prefix-stability property: garbage at exactly the size
    // limit used to report HeadTooLarge while its first byte reported BadStartLine.
    assert_eq!(parse_response(&[0u8; 16], 16), Err(HttpError::BadStartLine));
    assert_eq!(parse_response(&[0u8; 1], 16), Err(HttpError::BadStartLine));
    assert_eq!(parse(b"HTT"), Ok(None));
}

#[test]
fn rejects_malformed_and_ambiguous_headers() {
    let p = |h: &str| parse(format!("HTTP/1.1 200 OK\r\n{h}\r\n\r\n").as_bytes());
    assert_eq!(p("NoColon"), Err(HttpError::BadHeader));
    assert_eq!(p("Bad Name: x"), Err(HttpError::BadHeader));
    assert_eq!(
        p("Name : x"),
        Err(HttpError::BadHeader),
        "space before colon"
    );
    assert_eq!(
        p("A: 1\r\n folded"),
        Err(HttpError::BadHeader),
        "obsolete line folding"
    );
    assert_eq!(p("A: bad\x00value"), Err(HttpError::BadHeader));
    assert_eq!(p("A: bare\rcr"), Err(HttpError::BadHeader));
    assert_eq!(p("Content-Length: 1x"), Err(HttpError::BadContentLength));
    assert_eq!(p("Content-Length: -1"), Err(HttpError::BadContentLength));
    assert_eq!(p("Content-Length: "), Err(HttpError::BadContentLength));
    assert_eq!(
        p("Content-Length: 99999999999999999999"),
        Err(HttpError::BadContentLength)
    );
    assert_eq!(
        p("Content-Length: 18446744073709551616"),
        Err(HttpError::BadContentLength),
        "u64::MAX + 1"
    );
    assert_eq!(
        p("Content-Length: 18446744073709551615")
            .unwrap()
            .unwrap()
            .fields
            .content_length,
        Some(u64::MAX)
    );
    assert_eq!(
        p("Content-Length: 00000000000000000000000000000000000007")
            .unwrap()
            .unwrap()
            .fields
            .content_length,
        Some(7),
        "leading zeros"
    );
    assert_eq!(
        p("Content-Length: 10000000000000000000")
            .unwrap()
            .unwrap()
            .fields
            .content_length,
        Some(10_000_000_000_000_000_000)
    );
    assert_eq!(
        p("Content-Length: 5\r\nContent-Length: 6"),
        Err(HttpError::BadContentLength)
    );
    assert_eq!(
        p("Content-Length: 5\r\nContent-Length: 5")
            .unwrap()
            .unwrap()
            .fields
            .content_length,
        Some(5)
    );
    assert_eq!(
        p("Transfer-Encoding: chunked\r\nContent-Length: 5"),
        Err(HttpError::BadFraming)
    );
    assert_eq!(p("Transfer-Encoding: gzip"), Err(HttpError::BadFraming));
    assert!(
        p("Transfer-Encoding: Chunked")
            .unwrap()
            .unwrap()
            .fields
            .chunked
    );
}

#[test]
fn enforces_limits() {
    let many: String = (0..HEADERS_MAX + 1)
        .map(|i| format!("H{i}: v\r\n"))
        .collect();
    assert_eq!(
        parse(format!("HTTP/1.1 200 OK\r\n{many}\r\n").as_bytes()),
        Err(HttpError::TooManyHeaders)
    );
    let exactly: String = (0..HEADERS_MAX).map(|i| format!("H{i}: v\r\n")).collect();
    assert!(
        parse(format!("HTTP/1.1 200 OK\r\n{exactly}\r\n").as_bytes())
            .unwrap()
            .is_some()
    );
    let long = format!("HTTP/1.1 200 OK\r\nA: {}\r\n\r\n", "x".repeat(200));
    assert_eq!(
        parse_response(long.as_bytes(), 100),
        Err(HttpError::HeadTooLarge)
    );
    assert_eq!(
        parse_response(&long.as_bytes()[..50], 100),
        Ok(None),
        "below the limit it just needs more bytes"
    );
    assert_eq!(
        parse_response(&long.as_bytes()[..100], 100),
        Err(HttpError::HeadTooLarge)
    );
}

#[test]
fn writes_a_request_head_the_parser_accepts() {
    let mut buf = [0u8; 256];
    let n = HeadWriter::request(&mut buf, "PUT", "/bucket/key%20with%20space?x=1")
        .header("Host", b"s3.example.com")
        .header_u64("Content-Length", 8_388_608)
        .header("x-amz-content-sha256", b"UNSIGNED-PAYLOAD")
        .finish()
        .unwrap();
    assert_eq!(&buf[..n], &b"PUT /bucket/key%20with%20space?x=1 HTTP/1.1\r\nHost: s3.example.com\r\nContent-Length: 8388608\r\nx-amz-content-sha256: UNSIGNED-PAYLOAD\r\n\r\n"[..]);
    let r = parse_request(&buf[..n], LIMIT).unwrap().unwrap();
    assert_eq!(
        (r.method(&buf), r.target(&buf), r.fields.content_length),
        (
            &b"PUT"[..],
            &b"/bucket/key%20with%20space?x=1"[..],
            Some(8_388_608)
        )
    );
    assert_eq!(r.fields.header(&buf, "host"), Some(&b"s3.example.com"[..]));
}

#[test]
fn writer_refuses_injection_and_overflow() {
    let mut buf = [0u8; 256];
    assert_eq!(
        HeadWriter::request(&mut buf, "GET", "/a b").finish(),
        Err(HttpError::BadRequest),
        "space in target"
    );
    assert_eq!(
        HeadWriter::request(&mut buf, "GET", "/a\r\nX: y").finish(),
        Err(HttpError::BadRequest)
    );
    assert_eq!(
        HeadWriter::request(&mut buf, "GET", "/")
            .header("X", b"v\r\nInjected: 1")
            .finish(),
        Err(HttpError::BadRequest)
    );
    assert_eq!(
        HeadWriter::request(&mut buf, "GET", "/")
            .header("Bad Name", b"v")
            .finish(),
        Err(HttpError::BadRequest)
    );
    assert_eq!(
        HeadWriter::request(&mut buf, "G ET", "/").finish(),
        Err(HttpError::BadRequest)
    );
    let mut small = [0u8; 20];
    assert_eq!(
        HeadWriter::request(&mut small, "GET", "/")
            .header("Host", b"example.com")
            .finish(),
        Err(HttpError::BadRequest)
    );
    let n = HeadWriter::response(&mut buf, 503, "Slow Down")
        .header_u64("Content-Length", 0)
        .finish()
        .unwrap();
    assert_eq!(parse(&buf[..n]).unwrap().unwrap().status, 503);
}

#[test]
fn random_mutations_never_panic() {
    let mut s = 0x1319_8a2e_0370_7344u64;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for _ in 0..200_000 {
        let mut m = OK.to_vec();
        for _ in 0..1 + rnd() % 4 {
            let i = (rnd() % m.len() as u64) as usize;
            m[i] = rnd() as u8;
        }
        m.truncate(1 + (rnd() % m.len() as u64) as usize);
        if let Ok(Some(h)) = parse(&m) {
            assert!(h.fields.head_len <= m.len());
            let _ = h.fields.header(&m, "content-length");
        }
        let _ = parse_request(&m, LIMIT);
    }
}
