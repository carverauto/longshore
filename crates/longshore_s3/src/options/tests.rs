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
