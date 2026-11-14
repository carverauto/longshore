// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nContent-Type:  text/plain \r\nx-amz-meta-Owner: a:b\r\nETag: \"abc\"\r\n\r\n";

#[test]
fn finds_headers_whatever_their_case() {
    assert_eq!(header_in(HEAD, "content-type"), Some(&b"text/plain"[..]));
    assert_eq!(header_in(HEAD, "X-AMZ-META-OWNER"), Some(&b"a:b"[..]));
    assert_eq!(header_in(HEAD, "etag"), Some(&b"\"abc\""[..]));
    assert_eq!(header_in(HEAD, "content"), None);
    assert_eq!(header_in(HEAD, "x-amz-meta-other"), None);
}

#[test]
fn the_status_line_and_a_cut_head_are_not_headers() {
    assert_eq!(header_in(HEAD, "HTTP/1.1 200 OK"), None);
    assert_eq!(
        header_in(b"HTTP/1.1 200 OK\r\nContent-Le", "content-length"),
        None
    );
    assert_eq!(header_in(b"", "etag"), None);
}
