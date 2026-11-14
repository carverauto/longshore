// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The request that proves new credentials before they are adopted: a
//! listing of the whole bucket, of a prefix, or nothing.

mod creds_world;
use creds_world::*;
use longshore::io::alloc_guard::Guard;
use longshore::sim::Faults;
use longshore::store::s3::{KeyPrefix, Outcome, Validate};

#[global_allocator]
static ALLOC: Guard = Guard;

/// A world whose server lets keys list only under `ugdb/`, with the file
/// already naming the new key, which the server accepts.
fn restricted(validate: Validate) -> World {
    restricted_to("ugdb/", validate)
}

fn restricted_to(prefix: &'static str, validate: Validate) -> World {
    let mut w = world_validating(9, Faults::default(), BUF, validate);
    w.server.allow_listing_only_under(prefix);
    w.server.allow_access_keys(&[KEY_A, KEY_B]);
    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
    w.settle(3 * REREAD_MS);
    w
}

#[test]
fn credentials_that_may_not_list_the_whole_bucket_are_never_adopted_by_default() {
    let w = restricted(Validate::List(KeyPrefix::EMPTY));
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_A, 0));
    assert!(
        w.s3.credential_failures >= 2,
        "every refused try is counted, so the caller can see it"
    );
}

#[test]
fn they_are_adopted_when_the_trial_listing_names_a_prefix_they_may_list() {
    let mut w = restricted(Validate::List(KeyPrefix::new("ugdb/cluster/").unwrap()));
    assert_eq!(
        (
            w.s3.access_key(),
            w.s3.credential_rotations,
            w.s3.credential_failures
        ),
        (KEY_B, 1, 0)
    );
    assert_eq!(w.store().outcome, Outcome::Stored);
}

#[test]
fn with_no_trial_request_the_file_is_adopted_as_it_is() {
    // The server would refuse any listing these keys made.
    let mut w = restricted(Validate::Nothing);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));
    assert_eq!(w.store().outcome, Outcome::Stored);
    // Nothing proves them: a key the server refuses is adopted too, and
    // operations fail until the file is corrected.
    w.server.allow_access_keys(&[KEY_B]);
    w.io.set_file(w.creds, Some(file("AKIDWRONG").as_bytes()));
    w.settle(2 * REREAD_MS);
    assert_eq!(w.s3.access_key(), "AKIDWRONG");
    assert!(matches!(w.store().outcome, Outcome::Fatal(_)));
    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
    w.settle(2 * REREAD_MS);
    assert_eq!(w.store().outcome, Outcome::Stored);
}

#[test]
fn a_prefix_longer_than_the_limit_is_refused() {
    assert!(KeyPrefix::new(&"p".repeat(KeyPrefix::MAX)).is_some());
    assert!(KeyPrefix::new(&"p".repeat(KeyPrefix::MAX + 1)).is_none());
    assert_eq!(KeyPrefix::new("ugdb/").unwrap().as_str(), "ugdb/");
}

/// Every prefix `KeyPrefix::new` accepts can be sent, even at the limit
/// and with every byte percent-encoded to three: one of `/` bytes, and one
/// of three-byte characters.
#[test]
fn a_prefix_at_the_limit_is_listed_however_long_it_encodes() {
    let slashes: &'static str = "/".repeat(KeyPrefix::MAX).leak();
    let wide: &'static str = format!("{}/", "\u{9375}".repeat((KeyPrefix::MAX - 1) / 3)).leak();
    for prefix in [slashes, wide] {
        assert_eq!(prefix.len(), KeyPrefix::MAX);
        let mut w = restricted_to(prefix, Validate::List(KeyPrefix::new(prefix).unwrap()));
        assert_eq!(
            (
                w.s3.access_key(),
                w.s3.credential_rotations,
                w.s3.credential_failures
            ),
            (KEY_B, 1, 0),
            "prefix of {} bytes",
            prefix.len()
        );
        assert_eq!(w.store().outcome, Outcome::Stored);
    }
}
