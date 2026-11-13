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

/// One page of a delimiter listing: its keys, its common prefixes, and the
/// token to resume it with if it was cut short.
fn delimited_page(
    w: &mut World,
    token: Option<&str>,
    max: u16,
) -> (Vec<String>, Vec<String>, Option<String>) {
    match token {
        None => w.s3.list_with(&mut w.io, 901, "", None, max, Some("/"), RX),
        Some(t) => {
            w.s3.list_continue(&mut w.io, 901, "", t, max, Some("/"), RX)
        }
    }
    .unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("list failed: {:?}", w.done.last())
    };
    let xml = w.io.buf(RX)[..len as usize].to_vec();
    let listing = Listing::parse(&xml).expect("the server's listing must parse");
    let text = |raw: &[u8]| {
        let mut out = [0u8; 256];
        let n = unescape(raw, &mut out).unwrap();
        String::from_utf8(out[..n].to_vec()).unwrap()
    };
    let keys = listing
        .entries()
        .map(|e| text(e.unwrap().key_raw))
        .collect();
    let prefixes = listing
        .common_prefixes()
        .map(|p| text(p.unwrap()))
        .collect();
    assert_eq!(
        listing.is_truncated,
        listing.next_token_raw.is_some(),
        "a token comes with a truncated page and only then"
    );
    (keys, prefixes, listing.next_token_raw.map(text))
}

#[test]
fn a_delimiter_listing_pages_by_continuation_token_past_common_prefixes() {
    let mut w = world(31, Faults::default(), 3);
    for (i, key) in KEYS.iter().enumerate() {
        w.fill_tx(10, 0);
        w.s3.store(&mut w.io, i as u64, key, TX, 10, Checksum::None)
            .unwrap();
        assert_eq!(w.last().outcome, Outcome::Stored, "{key}");
    }
    // One entry per page, so every page but the last ends on something,
    // and the first two end on a common prefix.
    let (mut keys, mut prefixes, mut pages) = (Vec::new(), Vec::new(), 0);
    let mut token: Option<String> = None;
    loop {
        let (k, p, next) = delimited_page(&mut w, token.as_deref(), 1);
        assert_eq!(k.len() + p.len(), 1, "one entry per page");
        keys.extend(k);
        prefixes.extend(p);
        pages += 1;
        match next {
            Some(t) => token = Some(t),
            None => break,
        }
        assert!(pages < 10, "the listing does not end");
    }
    // Each prefix once, though each stands for several keys.
    assert_eq!(prefixes, ["logs/", "other/"]);
    assert_eq!(keys, ["zz top"]);
    assert_eq!(pages, 3);
}

#[test]
fn a_continuation_token_is_not_a_key_and_an_empty_one_is_refused() {
    let mut w = world(32, Faults::default(), 3);
    for (i, key) in KEYS.iter().enumerate() {
        w.fill_tx(10, 0);
        w.s3.store(&mut w.io, i as u64, key, TX, 10, Checksum::None)
            .unwrap();
        assert_eq!(w.last().outcome, Outcome::Stored);
    }
    let (_, prefixes, token) = delimited_page(&mut w, None, 1);
    assert_eq!(prefixes, ["logs/"]);
    let token = token.expect("a truncated page has a token");
    assert!(!KEYS.contains(&token.as_str()) && token != "logs/");
    // Passed as a key to start after, it does not resume the listing:
    // the token route gives the next prefix, and this does not.
    w.s3.list_with(&mut w.io, 902, "", Some(&token), 1, Some("/"), RX)
        .unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!()
    };
    let xml = w.io.buf(RX)[..len as usize].to_vec();
    let listing = Listing::parse(&xml).unwrap();
    assert_eq!(listing.common_prefixes().count(), 0);
    let (_, resumed, _) = delimited_page(&mut w, Some(&token), 1);
    assert_eq!(resumed, ["other/"]);
    assert_eq!(
        w.s3.list_continue(&mut w.io, 903, "", "", 1, Some("/"), RX),
        Err(SubmitError::BadKey)
    );
    let long = "x".repeat(longshore::store::s3::LIST_TOKEN_ENCODED_MAX + 1);
    assert_eq!(
        w.s3.list_continue(&mut w.io, 904, "", &long, 1, Some("/"), RX),
        Err(SubmitError::BadKey)
    );
}
