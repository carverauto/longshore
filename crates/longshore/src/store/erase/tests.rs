// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn writes_keys_with_markup_escaped() {
    let mut out = [0u8; 256];
    let n = write_manifest(&mut out, &["a/b", "x&y<z>\"q\""]).unwrap();
    assert_eq!(
        core::str::from_utf8(&out[..n]).unwrap(),
        "<Delete><Quiet>true</Quiet>\
         <Object><Key>a/b</Key></Object>\
         <Object><Key>x&amp;y&lt;z&gt;&quot;q&quot;</Key></Object>\
         </Delete>"
    );
}

#[test]
fn refuses_a_manifest_a_store_would_refuse_or_that_does_not_fit() {
    let mut out = vec![0u8; 64 * 1024];
    assert!(write_manifest(&mut out, &[]).is_none());
    assert!(write_manifest(&mut out, &["a", ""]).is_none());
    let many = vec!["k"; KEYS_MAX + 1];
    assert!(write_manifest(&mut out, &many).is_none());
    assert!(write_manifest(&mut out, &many[..KEYS_MAX]).is_some());
    let need = write_manifest(&mut out, &["key"]).unwrap();
    assert!(write_manifest(&mut out[..need - 1], &["key"]).is_none());
    assert!(write_manifest(&mut out[..need], &["key"]).is_some());
}

#[test]
fn reads_the_failures_of_a_real_shaped_response() {
    let doc = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Error><Key>locked/a&amp;b</Key><Code>AccessDenied</Code><Message>Access Denied</Message></Error><Error><Key>c</Key><Code>InternalError</Code></Error></DeleteResult>";
    let failures: Vec<_> = Failures::parse(doc).unwrap().map(|f| f.unwrap()).collect();
    assert_eq!(
        failures,
        [
            Failure {
                key_raw: b"locked/a&amp;b",
                code: b"AccessDenied"
            },
            Failure {
                key_raw: b"c",
                code: b"InternalError"
            },
        ]
    );
}

#[test]
fn an_empty_result_means_everything_was_erased() {
    for doc in [
        &b"<DeleteResult></DeleteResult>"[..],
        b"<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"/>",
        b"<DeleteResult/>",
        b"<DeleteResult><Deleted><Key>k</Key></Deleted></DeleteResult>",
    ] {
        assert_eq!(Failures::parse(doc).unwrap().count(), 0);
    }
}

#[test]
fn rejects_what_is_not_a_result_or_is_cut_short() {
    assert_eq!(
        Failures::parse(b"<Error><Code>x</Code></Error>").err(),
        Some(ListError::NotAListing)
    );
    assert_eq!(
        Failures::parse(b"<DeleteResults></DeleteResults>").err(),
        Some(ListError::NotAListing)
    );
    assert_eq!(
        Failures::parse(b"<DeleteResult><Error>").err(),
        Some(ListError::Malformed)
    );
    let cut = Failures::parse(b"<DeleteResult><Error><Key>k</Key></Error></DeleteResult>").unwrap();
    assert_eq!(cut.collect::<Vec<_>>(), [Err(ListError::Malformed)]);
}
