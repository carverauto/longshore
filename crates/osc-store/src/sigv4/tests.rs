// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn uri_encoding_rules() {
    let enc = |s: &str, slash: bool| {
        let mut out = [0u8; 128];
        let n = uri_encode(s.as_bytes(), slash, &mut out).unwrap();
        String::from_utf8(out[..n].to_vec()).unwrap()
    };
    assert_eq!(
        enc("/photos/Jan sample.jpg", false),
        "/photos/Jan%20sample.jpg"
    );
    assert_eq!(enc("a/b", true), "a%2Fb");
    assert_eq!(enc("AZaz09-._~", true), "AZaz09-._~");
    assert_eq!(enc("+=&*'()", true), "%2B%3D%26%2A%27%28%29");
    assert_eq!(
        enc("é", false),
        "%C3%A9",
        "UTF-8 bytes are encoded individually, uppercase hex"
    );
    assert_eq!(uri_encode(b"a b", false, &mut [0u8; 4]), None);
}
