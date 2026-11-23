// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The head parsers at their boundaries: the size limit to the byte, the
//! framing headers in every order, near-miss tokens, and every way a
//! request line can be wrong.

use longshore_http_client::head::{parse_request, parse_response, HeadWriter, HttpError};

const LIMIT: usize = 2048;

/// A response head with these header lines.
fn response(headers: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\n{headers}\r\n\r\n").into_bytes()
}

#[cfg(test)]
fn status_of(line: &str) -> Result<u16, HttpError> {
    parse_response(format!("{line}\r\n\r\n").as_bytes(), LIMIT)
        .map(|h| h.expect("a complete head").status)
}

#[test]
fn the_size_limit_is_exact() {
    for limit in [64usize, 100, 2048] {
        // "HTTP/1.1 200 OK\r\nA: " + value + "\r\n\r\n" is 24 bytes around the value.
        let head = |len: usize| response(&format!("A: {}", "v".repeat(len - 24)));
        let fits = head(limit);
        assert_eq!(fits.len(), limit);
        let parsed = parse_response(&fits, limit).unwrap().unwrap();
        assert_eq!(parsed.fields.head_len, limit);
        assert_eq!(
            parse_response(&head(limit + 1), limit),
            Err(HttpError::HeadTooLarge),
            "one byte over {limit}"
        );
        // The same head arriving a byte at a time: undecided until complete.
        for seen in 8..limit {
            assert_eq!(parse_response(&fits[..seen], limit), Ok(None), "{seen}");
        }
        // And for requests.
        let req =
            |len: usize| format!("GET /{} HTTP/1.1\r\n\r\n", "k".repeat(len - 18)).into_bytes();
        assert!(parse_request(&req(limit), limit).unwrap().is_some());
        assert_eq!(
            parse_request(&req(limit + 1), limit),
            Err(HttpError::HeadTooLarge)
        );
        assert_eq!(
            parse_request(&vec![b'G'; limit], limit),
            Err(HttpError::HeadTooLarge),
            "a request that never ends"
        );
    }
}

#[test]
fn framing_headers_are_refused_in_every_ambiguous_form() {
    let p = |h: &str| parse_response(&response(h), LIMIT).map(|h| h.unwrap().fields);
    for bad in [
        "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked",
        "Transfer-Encoding: gzip, chunked",
        "Transfer-Encoding: chunked, gzip",
        "Transfer-Encoding: gzip",
        "Transfer-Encoding:",
        "Content-Length: 5\r\nTransfer-Encoding: chunked",
        "Transfer-Encoding: chunked\r\nContent-Length: 5",
        "Transfer-Encoding: chunked\r\nContent-Length: 0",
    ] {
        assert_eq!(p(bad).err(), Some(HttpError::BadFraming), "{bad}");
    }
    let f = p("transfer-ENCODING:  CHUNKED\t").unwrap();
    assert!(f.chunked && f.content_length.is_none());
    for bad in [
        "Content-Length: 5\r\nContent-Length: 6",
        "Content-Length: 5, 5",
        "Content-Length: +5",
        "Content-Length: 0x5",
        "Content-Length: 5 5",
        "Content-Length:",
    ] {
        assert_eq!(p(bad).err(), Some(HttpError::BadContentLength), "{bad}");
    }
    assert_eq!(
        p("Content-Length: 7\r\nContent-Length: 7")
            .unwrap()
            .content_length,
        Some(7),
        "a repeated equal length is one length"
    );
    assert_eq!(p("Content-Length: 007").unwrap().content_length, Some(7));
}

/// `close` and `keep-alive` are whole tokens. A parser that looked for the
/// letters anywhere in the value would pass a test that only tries the
/// exact words.
#[test]
fn connection_tokens_are_matched_whole() {
    let close = |version: &str, h: &str| {
        parse_response(format!("{version} 200 OK\r\n{h}\r\n\r\n").as_bytes(), LIMIT)
            .unwrap()
            .unwrap()
            .fields
            .close
    };
    for not_close in [
        "Connection: closed",
        "Connection: x-close",
        "Connection: close-notify",
        "Connection: keep-alive, upgrade",
        "X-Connection: close",
        "Connection:",
    ] {
        assert!(!close("HTTP/1.1", not_close), "{not_close}");
    }
    for is_close in [
        "Connection: close",
        "Connection: CLOSE",
        "Connection: upgrade ,\tclose",
        "Connection: keep-alive\r\nConnection: close",
    ] {
        assert!(close("HTTP/1.1", is_close), "{is_close}");
    }
    // HTTP/1.0 closes unless it says keep-alive, as a whole token.
    assert!(close("HTTP/1.0", "A: b"));
    assert!(close("HTTP/1.0", "Connection: keep-alive-ish"));
    assert!(close("HTTP/1.0", "Connection: not-keep-alive"));
    assert!(!close("HTTP/1.0", "Connection: Keep-Alive"));
    assert!(close("HTTP/1.0", "Connection: keep-alive, close"));
}

#[test]
fn header_names_and_values_at_their_edges() {
    let buf = response("A:\tv\t\r\nEmpty:\r\nDup: first\r\nDUP: second\r\nSpaces:   \r\nB: a b");
    let f = parse_response(&buf, LIMIT).unwrap().unwrap().fields;
    assert_eq!(f.header(&buf, "a"), Some(&b"v"[..]));
    assert_eq!(f.header(&buf, "empty"), Some(&b""[..]));
    assert_eq!(f.header(&buf, "spaces"), Some(&b""[..]));
    assert_eq!(f.header(&buf, "dup"), Some(&b"first"[..]), "the first wins");
    assert_eq!(f.header(&buf, "b"), Some(&b"a b"[..]));
    assert_eq!(
        f.headers(&buf, "dup").collect::<Vec<_>>(),
        [&b"first"[..], b"second"],
        "every line of a name, in order"
    );
    assert_eq!(f.headers(&buf, "A").collect::<Vec<_>>(), [&b"v"[..]]);
    assert_eq!(f.headers(&buf, "absent").count(), 0);
    assert_eq!(f.header(&buf, "absent"), None);
    assert_eq!(f.header(&buf, ""), None);
    let p = |h: &str| parse_response(&response(h), LIMIT).map(|_| ());
    for bad in [
        ": x",
        "A: a\x7fb",
        "A b: x",
        "A\t: x",
        "\tA: x",
        "A: x\x0b",
        "é: x",
    ] {
        assert_eq!(p(bad), Err(HttpError::BadHeader), "{bad:?}");
    }
    // Bytes above ASCII are legal in a value.
    assert_eq!(p("A: caf\u{e9}"), Ok(()));
}

#[test]
fn status_lines_at_their_edges() {
    assert_eq!(status_of("HTTP/1.1 100 Continue"), Ok(100));
    assert_eq!(status_of("HTTP/1.1 999 Whatever"), Ok(999));
    assert_eq!(status_of("HTTP/1.1 200"), Ok(200));
    assert_eq!(status_of("HTTP/1.1 200 "), Ok(200));
    assert_eq!(status_of("HTTP/1.0 204 No\tContent"), Ok(204));
    for bad in [
        "HTTP/1.1 099 Low",
        "HTTP/1.1 000 Zero",
        "HTTP/1.1 200OK",
        "HTTP/1.1 2x0 OK",
        "HTTP/1.1 20 OK",
        "HTTP/1.1 2000 OK",
        "HTTP/1.1  200 OK",
        "HTTP/1.1\t200 OK",
        "HTTP/1.1 200 O\x01K",
        "HTTP/1.2 200 OK",
        "HTTP/2.0 200 OK",
        "http/1.1 200 OK",
        "HTTP/1.1 -20 OK",
    ] {
        assert_eq!(status_of(bad), Err(HttpError::BadStartLine), "{bad:?}");
    }
}

#[test]
fn request_lines_are_refused_in_every_malformed_form() {
    let p = |line: &str| {
        let buf = format!("{line}\r\n\r\n").into_bytes();
        parse_request(&buf, LIMIT).map(|h| {
            let h = h.expect("a complete head");
            (
                String::from_utf8_lossy(h.method(&buf)).into_owned(),
                String::from_utf8_lossy(h.target(&buf)).into_owned(),
            )
        })
    };
    assert_eq!(p("GET / HTTP/1.1"), Ok(("GET".into(), "/".into())));
    assert_eq!(
        p("M-SEARCH /a?b=c%20d HTTP/1.0"),
        Ok(("M-SEARCH".into(), "/a?b=c%20d".into()))
    );
    for bad in [
        "GET",
        "GET /",
        " / HTTP/1.1",
        "GET  HTTP/1.1",
        "G@T / HTTP/1.1",
        "GET /a\x7f HTTP/1.1",
        "GET /a\tb HTTP/1.1",
        "GET /a b HTTP/1.1",
        "GET / HTTP/2",
        "GET / HTTP/1.1 ",
        "GET / http/1.1",
        "",
    ] {
        assert_eq!(p(bad).err(), Some(HttpError::BadStartLine), "{bad:?}");
    }
    // The header rules are the response parser's.
    let buf = b"PUT /k HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert_eq!(parse_request(buf, LIMIT), Err(HttpError::BadFraming));
}

/// The writer fills a buffer to its last byte and refuses one byte less.
#[test]
fn the_writer_fits_exactly_or_not_at_all() {
    let write = |buf: &mut [u8]| {
        let mut w = HeadWriter::request(buf, "PUT", "/key");
        let _: &mut HeadWriter<'_> = w.header("Host", b"objects.test");
        let _: &mut HeadWriter<'_> = w.header_u64("Content-Length", u64::MAX);
        w.finish()
    };
    let mut big = [0u8; 256];
    let n = write(&mut big).unwrap();
    let expected =
        "PUT /key HTTP/1.1\r\nHost: objects.test\r\nContent-Length: 18446744073709551615\r\n\r\n";
    assert_eq!(&big[..n], expected.as_bytes());
    let mut exact = vec![0u8; n];
    assert_eq!(write(&mut exact), Ok(n));
    assert_eq!(exact, expected.as_bytes());
    for short in 0..n {
        let mut buf = vec![0u8; short];
        assert_eq!(write(&mut buf), Err(HttpError::BadRequest), "{short} bytes");
    }
    let parsed = parse_request(&exact, LIMIT).unwrap().unwrap();
    assert_eq!(parsed.fields.content_length, Some(u64::MAX));
}
