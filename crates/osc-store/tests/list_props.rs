// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) for the list-response parser.

use hegel::generators as gs;
use hegel::TestCase;
use osc_store::list::{unescape, Listing};

fn escape(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&#34;".to_string(),
            '\'' => "&#x27;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// Any set of keys and sizes survives being written as a listing and parsed
/// back, whatever characters the keys contain and whatever unrelated fields
/// surround them.
#[hegel::test]
fn listings_round_trip(tc: TestCase) {
    let entries: Vec<(String, u64)> = (0..tc
        .draw(gs::integers::<usize>().min_value(0).max_value(8)))
        .map(|_| {
            let mut key = tc.draw(gs::text().min_size(1).max_size(40));
            // Control characters cannot appear in XML 1.0 at all.
            key.retain(|c| !c.is_control());
            if key.is_empty() {
                key.push('k');
            }
            (key, tc.draw(gs::integers::<u64>()))
        })
        .collect();
    let truncated = tc.draw(gs::booleans());
    let summary_first = tc.draw(gs::booleans());
    let summary = format!(
        "<Name>bucket</Name><IsTruncated>{truncated}</IsTruncated><KeyCount>{}</KeyCount>",
        entries.len()
    );
    let contents: String = entries.iter().map(|(k, size)| format!("<Contents><Key>{}</Key><LastModified>2026-11-12T00:00:00.000Z</LastModified><ETag>&#34;abc&#34;</ETag><Size>{size}</Size><StorageClass>STANDARD</StorageClass></Contents>", escape(k))).collect();
    let xml = if summary_first {
        format!("<?xml version=\"1.0\"?><ListBucketResult xmlns=\"x\">{summary}{contents}</ListBucketResult>")
    } else {
        format!("<ListBucketResult>{contents}{summary}</ListBucketResult>")
    };

    let listing = Listing::parse(xml.as_bytes()).expect("a well-formed listing must parse");
    assert_eq!(listing.is_truncated, truncated);
    let got: Vec<(String, u64)> = listing
        .entries()
        .map(|e| {
            let e = e.expect("well-formed entry");
            let mut out = vec![0u8; e.key_raw.len()];
            let n = unescape(e.key_raw, &mut out).expect("valid references");
            (String::from_utf8(out[..n].to_vec()).unwrap(), e.size)
        })
        .collect();
    assert_eq!(got, entries);
}

/// Arbitrary bytes never panic the parser, and whatever it yields points
/// inside the input.
#[hegel::test]
fn arbitrary_documents_are_safe(tc: TestCase) {
    let mut data = tc.draw(gs::binary().max_size(400));
    if tc.draw(gs::booleans()) {
        // Give it a plausible start so the interesting paths are reached.
        let mut doc = b"<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>".to_vec();
        doc.append(&mut data);
        data = doc;
    }
    if let Ok(listing) = Listing::parse(&data) {
        for e in listing.entries().flatten() {
            assert!(!e.key_raw.is_empty());
            let mut out = vec![0u8; e.key_raw.len()];
            let _ = unescape(e.key_raw, &mut out);
        }
    }
    let mut out = vec![0u8; data.len()];
    if let Some(n) = unescape(&data, &mut out) {
        assert!(n <= data.len());
    }
}
