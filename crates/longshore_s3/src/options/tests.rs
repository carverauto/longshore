// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn wire(o: &Options, prefix: &'static str) -> Vec<String> {
    o.headers(prefix)
        .map(|(p, n, v)| format!("{p}{n}: {v}"))
        .collect()
}

#[test]
fn headers_come_out_as_each_vendor_names_them() {
    let o = Options::new()
        .content_type("text/plain; charset=utf-8")
        .and_then(|o| o.metadata("Owner", "team a"))
        .and_then(|o| o.storage_class("STANDARD_IA"))
        .and_then(|o| o.header("X-Amz-Server-Side-Encryption", "AES256"))
        .unwrap();
    assert_eq!(
        wire(&o, "x-amz-"),
        [
            "content-type: text/plain; charset=utf-8",
            "x-amz-meta-owner: team a",
            "x-amz-storage-class: STANDARD_IA",
            "x-amz-server-side-encryption: AES256",
        ]
    );
    assert_eq!(wire(&o, "x-goog-")[1], "x-goog-meta-owner: team a");
    assert!(Options::new().is_empty() && !o.is_empty());
}

#[test]
fn refuses_what_would_break_a_request() {
    let o = Options::new();
    assert!(o.header("host", "elsewhere").is_none());
    assert!(o.header("Authorization", "x").is_none());
    assert!(o.header("x-amz-date", "x").is_none());
    assert!(o.header("bad name", "x").is_none());
    assert!(o.header("", "x").is_none());
    assert!(o.metadata("a:b", "x").is_none());
    assert!(o.content_type("text/plain\r\nx-evil: 1").is_none());
    assert!(o.metadata("k", "caf\u{e9}").is_none());
    // The same header twice.
    let once = o.content_type("a/b").unwrap();
    assert!(once.content_type("c/d").is_none());
    assert!(once.header("Content-Type", "c/d").is_none());
    let meta = o.metadata("k", "1").unwrap();
    assert!(meta.metadata("K", "2").is_none());
}

#[test]
fn refuses_what_does_not_fit() {
    let mut o = Options::new();
    for k in 0..OPTIONS_MAX {
        o = o.metadata(&format!("k{k}"), "v").unwrap();
    }
    assert!(o.metadata("one-more", "v").is_none());
    let long = "v".repeat(OPTIONS_BYTES);
    assert!(Options::new().metadata("k", &long).is_none());
    assert!(Options::new()
        .metadata("k", &long[..OPTIONS_BYTES - 6])
        .is_some());
    assert!(Options::new().header(&"n".repeat(NAME_MAX), "v").is_none());
}

/// There is one way to say each thing. A header given by name may not
/// repeat one given through `metadata` or `storage_class`, in either order
/// and for either vendor's prefix: the request would carry it twice, and a
/// store takes one of the two or refuses both.
#[test]
fn a_header_given_by_name_may_not_repeat_one_given_by_its_method() {
    let with_metadata = Options::new().metadata("owner", "a").unwrap();
    assert!(with_metadata.header("x-amz-meta-owner", "b").is_none());
    assert!(with_metadata.header("X-Goog-Meta-Owner", "b").is_none());
    assert!(with_metadata.header("x-amz-meta-other", "b").is_some());
    let with_class = Options::new().storage_class("STANDARD").unwrap();
    assert!(with_class
        .header("x-amz-storage-class", "GLACIER")
        .is_none());
    assert!(with_class
        .header("x-goog-storage-class", "COLDLINE")
        .is_none());
    // The other order.
    let by_name = Options::new().header("x-amz-meta-owner", "b").unwrap();
    assert!(by_name.metadata("owner", "a").is_none());
    assert!(by_name.metadata("other", "a").is_some());
    let by_name = Options::new()
        .header("x-goog-storage-class", "COLDLINE")
        .unwrap();
    assert!(by_name.storage_class("STANDARD").is_none());
    // A name that only resembles one is its own header.
    let o = Options::new()
        .metadata("owner", "a")
        .and_then(|o| o.header("meta-owner", "c"))
        .and_then(|o| o.header("x-other-meta-owner", "d"))
        .expect("different headers");
    assert_eq!(
        wire(&o, "x-amz-"),
        [
            "x-amz-meta-owner: a",
            "meta-owner: c",
            "x-other-meta-owner: d"
        ]
    );
}

/// The checksum headers the store adds when an upload begins or a fetch is
/// verified are its own, like the checksum value itself.
#[test]
fn the_checksum_headers_the_store_sets_are_refused() {
    for added_by_the_store in [
        "x-amz-checksum-algorithm",
        "x-amz-checksum-type",
        "X-Amz-Checksum-Mode",
    ] {
        assert!(
            Options::new().header(added_by_the_store, "x").is_none(),
            "{added_by_the_store}"
        );
    }
}

#[test]
fn every_reserved_name_is_refused_in_any_case() {
    for name in RESERVED {
        assert!(Options::new().header(name, "x").is_none(), "{name}");
        assert!(
            Options::new()
                .header(&name.to_ascii_uppercase(), "x")
                .is_none(),
            "{name} in capitals"
        );
    }
}

#[test]
fn names_and_values_at_their_limits() {
    // A name leaves room for the vendor's prefix where it is built.
    let longest = NAME_MAX - 7;
    assert!(Options::new().header(&"n".repeat(longest), "v").is_some());
    assert!(Options::new()
        .header(&"n".repeat(longest + 1), "v")
        .is_none());
    // "meta-" counts towards a metadata name.
    assert!(Options::new()
        .metadata(&"n".repeat(longest - 5), "v")
        .is_some());
    assert!(Options::new()
        .metadata(&"n".repeat(longest - 4), "v")
        .is_none());
    // Names and values share the bytes: "meta-k" is six of them.
    assert!(Options::new()
        .metadata("k", &"v".repeat(OPTIONS_BYTES - 6))
        .is_some());
    assert!(Options::new()
        .metadata("k", &"v".repeat(OPTIONS_BYTES - 5))
        .is_none());
    // An empty value is a value; a tab is allowed inside one, a control
    // character or a byte past ASCII is not.
    let o = Options::new().header("x-empty", "").unwrap();
    assert_eq!(wire(&o, "x-amz-"), ["x-empty: "]);
    assert!(Options::new().header("x-tab", "a\tb").is_some());
    for bad in ["a\nb", "a\rb", "a\0b", "a\u{7f}b", "a\u{80}b"] {
        assert!(Options::new().header("x-bad", bad).is_none(), "{bad:?}");
    }
    // A refusal leaves the value it was asked of as it was.
    let o = Options::new().content_type("a/b").unwrap();
    assert!(o.header("bad name", "x").is_none());
    assert_eq!(wire(&o, "x-amz-"), ["content-type: a/b"]);
}
