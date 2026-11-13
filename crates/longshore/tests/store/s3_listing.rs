// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store against the in-tree HTTP server on one simulated `Io`, with
//! heap allocation forbidden while operations run.

mod common;
use common::*;

/// Keys as a store would be asked to hold them, including characters that
/// must be percent-encoded in the request and escaped in the listing.
const KEYS: [&str; 6] = [
    "logs/2026/a.bin",
    "logs/2026/b & c.bin",
    "logs/2026/d<e>.bin",
    "logs/2027/a.bin",
    "other/x",
    "zz top",
];

fn list_page(
    w: &mut World,
    prefix: &str,
    after: Option<&str>,
    max: u16,
) -> (Vec<(String, u64)>, bool) {
    w.s3.list(&mut w.io, 900, prefix, after, max, RX).unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("list failed: {:?}", w.done.last())
    };
    let xml = w.io.buf(RX)[..len as usize].to_vec();
    let listing = Listing::parse(&xml).expect("the server's listing must parse");
    let keys = listing
        .entries()
        .map(|e| {
            let e = e.unwrap();
            let mut out = [0u8; 256];
            let n = unescape(e.key_raw, &mut out).unwrap();
            (String::from_utf8(out[..n].to_vec()).unwrap(), e.size)
        })
        .collect();
    (keys, listing.is_truncated)
}

#[test]
fn list_pages_through_keys_by_prefix_in_order() {
    let mut w = world(30, Faults::default(), 3);
    for (i, key) in KEYS.iter().enumerate() {
        w.fill_tx(100 + i as u32, 0);
        w.s3.store(&mut w.io, i as u64, key, TX, 100 + i as u32, Checksum::None)
            .unwrap();
        assert_eq!(w.last().outcome, Outcome::Stored, "{key}");
    }
    // Everything, in one page.
    let (all, truncated) = list_page(&mut w, "", None, 1000);
    assert!(!truncated);
    assert_eq!(
        all,
        KEYS.iter()
            .enumerate()
            .map(|(i, k)| (k.to_string(), 100 + i as u64))
            .collect::<Vec<_>>(),
        "sorted, with sizes, special characters intact"
    );

    // A prefix, two keys at a time, paging by the last key seen.
    let mut seen = vec![];
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let (page, truncated) = list_page(&mut w, "logs/", after.as_deref(), 2);
        assert!(page.len() <= 2);
        seen.extend(page.iter().map(|p| p.0.clone()));
        pages += 1;
        if !truncated {
            break;
        }
        after = seen.last().cloned();
    }
    assert_eq!(seen, &KEYS[..4]);
    assert_eq!(
        pages, 2,
        "four keys, two per page: the second page is the last"
    );

    // Nothing matches.
    assert_eq!(list_page(&mut w, "nope/", None, 10), (vec![], false));
    // Listing after erasing reflects the erase.
    w.s3.erase(&mut w.io, 50, "logs/2026/a.bin").unwrap();
    assert_eq!(w.last().outcome, Outcome::Erased);
    assert_eq!(list_page(&mut w, "logs/2026/", None, 10).0.len(), 2);
}

#[test]
fn list_is_retried_like_any_other_request() {
    let mut w = world(31, Faults::default(), 4);
    w.fill_tx(10, 0);
    w.s3.store(&mut w.io, 1, "k", TX, 10, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.server.script(Behavior::Status(503));
    w.server.script(Behavior::CloseBeforeResponse);
    w.s3.list(&mut w.io, 2, "", None, 10, RX).unwrap();
    let d = w.last();
    assert!(
        matches!(d.outcome, Outcome::Listed { .. }) && d.attempts == 3,
        "{d:?}"
    );
    assert_eq!(
        w.s3.list(&mut w.io, 3, &"é".repeat(300), None, 10, RX),
        Err(SubmitError::BadKey),
        "prefix too long once encoded"
    );
}
