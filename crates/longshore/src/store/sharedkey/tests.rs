// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The development key every Azure Storage emulator ships with. It is
/// public and protects nothing.
const EMULATOR_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

fn string(req: &Request) -> String {
    let mut out = [0u8; STRING_MAX];
    let n = string_to_sign(req, &mut out).unwrap();
    String::from_utf8(out[..n].to_vec()).unwrap()
}

fn auth(req: &Request) -> String {
    let mut key = [0u8; KEY_MAX];
    let k = decode_key(EMULATOR_KEY, &mut key).unwrap();
    let mut out = [0u8; 128];
    let n = authorization(req, &key[..k], &mut out).unwrap();
    String::from_utf8(out[..n].to_vec()).unwrap()
}

/// The Get Container Metadata example from the Shared Key reference page,
/// with headers and query given out of order.
#[test]
fn matches_the_documented_string_to_sign() {
    let req = Request {
        method: "GET",
        content_length: 0,
        content_type: "",
        if_none_match: "",
        range: "",
        ms_headers: &[
            ("x-ms-version", "2015-02-21"),
            ("x-ms-date", "Fri, 26 Jun 2015 23:39:12 GMT"),
        ],
        account: "myaccount",
        path: "/mycontainer",
        query: &[
            ("timeout", "20"),
            ("restype", "container"),
            ("comp", "metadata"),
        ],
    };
    assert_eq!(string(&req), "GET\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\nx-ms-version:2015-02-21\n/myaccount/mycontainer\ncomp:metadata\nrestype:container\ntimeout:20");
}

/// Signatures computed independently (Python's hmac and base64) over
/// strings written out by hand from the reference's format.
#[test]
fn signs_a_conditional_store_and_a_ranged_listing() {
    let put = Request {
        method: "PUT",
        content_length: 1000,
        content_type: "application/octet-stream",
        if_none_match: "*",
        range: "",
        ms_headers: &[
            ("x-ms-version", "2021-08-06"),
            ("x-ms-date", "Thu, 12 Nov 2026 00:00:00 GMT"),
            ("x-ms-content-crc64", "AAECAwQFBgc="),
            ("x-ms-blob-type", "BlockBlob"),
        ],
        account: "devstoreaccount1",
        path: "/devstoreaccount1/c/dir/key%20one.bin",
        query: &[],
    };
    assert_eq!(
        auth(&put),
        "SharedKey devstoreaccount1:RbW9TuWFizrXjiz+s/tlneXSP4Tml/WpYSIBWPo9CH4="
    );
    let get = Request {
        method: "GET",
        content_length: 0,
        content_type: "",
        if_none_match: "",
        range: "bytes=100-199",
        ms_headers: &[
            ("x-ms-date", "Thu, 12 Nov 2026 00:00:00 GMT"),
            ("x-ms-version", "2021-08-06"),
        ],
        account: "devstoreaccount1",
        path: "/devstoreaccount1/c",
        query: &[
            ("restype", "container"),
            ("prefix", "a/b"),
            ("comp", "list"),
        ],
    };
    assert_eq!(
        auth(&get),
        "SharedKey devstoreaccount1:H3nu4vUIlnLpvaPAcogehwXE57twFbKei+/c7KayIWo="
    );
}

#[test]
fn refuses_what_does_not_fit() {
    let base = Request {
        method: "GET",
        content_length: 0,
        content_type: "",
        if_none_match: "",
        range: "",
        ms_headers: &[],
        account: "a",
        path: "/c",
        query: &[],
    };
    assert_eq!(
        string_to_sign(
            &Request {
                ms_headers: &[("x-ms-a", ""); HEADERS_MAX + 1],
                ..base
            },
            &mut [0; STRING_MAX]
        ),
        Err(SignError::TooManyHeaders)
    );
    assert_eq!(
        string_to_sign(
            &Request {
                query: &[("a", ""); QUERY_MAX + 1],
                ..base
            },
            &mut [0; STRING_MAX]
        ),
        Err(SignError::TooManyQueryParameters)
    );
    assert_eq!(string_to_sign(&base, &mut [0; 16]), Err(SignError::TooLong));
    assert_eq!(
        authorization(&base, b"k", &mut [0; 20]),
        Err(SignError::TooLong)
    );
}

#[test]
fn decodes_account_keys_and_refuses_malformed_ones() {
    let mut out = [0u8; KEY_MAX];
    assert_eq!(decode_key(EMULATOR_KEY, &mut out), Some(64));
    assert_eq!(&out[..4], &[0x11, 0xbc, 0xbc, 0xbd]);
    // Round trip through the encoder for every padding length.
    for len in 1..=KEY_MAX {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
        let mut text = [0u8; 88];
        let n = base64(&bytes, &mut text);
        let mut back = [0u8; KEY_MAX];
        assert_eq!(
            decode_key(core::str::from_utf8(&text[..n]).unwrap(), &mut back),
            Some(len)
        );
        assert_eq!(&back[..len], &bytes[..]);
    }
    for bad in [
        "", "abc", "ab=c", "a===", "ab!d", "=abc", "ab==cdef", "QQ=A", "QR==", "QUJ=",
    ] {
        assert_eq!(decode_key(bad, &mut out), None, "{bad:?}");
    }
    // One byte more than a key can be.
    let mut text = [0u8; 92];
    let n = base64(&[7u8; KEY_MAX + 1], &mut text);
    assert_eq!(
        decode_key(core::str::from_utf8(&text[..n]).unwrap(), &mut out),
        None
    );
}
