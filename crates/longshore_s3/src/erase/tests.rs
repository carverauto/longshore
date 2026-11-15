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

/// An XML parser turns a carriage return into a line feed unless it is
/// written as a reference, which would name another key.
#[test]
fn a_carriage_return_in_a_key_is_written_as_a_reference() {
    let mut out = [0u8; 128];
    let n = write_manifest(&mut out, &["a\rb\r\nc\td'e"]).unwrap();
    assert_eq!(
        core::str::from_utf8(&out[..n]).unwrap(),
        "<Delete><Quiet>true</Quiet>\
         <Object><Key>a&#13;b&#13;\nc\td'e</Key></Object>\
         </Delete>"
    );
}

#[test]
fn a_result_with_attributes_or_damage_after_its_name() {
    // A self-closing root is an empty result, with or without attributes.
    assert_eq!(Failures::parse(b"<DeleteResult />").unwrap().count(), 0);
    // Cut off at the name, or inside the opening tag.
    assert_eq!(
        Failures::parse(b"<DeleteResult").err(),
        Some(ListError::Malformed)
    );
    assert_eq!(
        Failures::parse(b"<DeleteResult xmlns=\"x\"").err(),
        Some(ListError::Malformed)
    );
    assert_eq!(
        Failures::parse(b"<DeleteResult><Error><Key>k</Key><Code>c</Code></Error>").err(),
        Some(ListError::Malformed),
        "no closing root"
    );
    // A failure with an empty key, or with no key at all, is damage; an
    // empty code is not.
    for (doc, expected) in [
        (
            &b"<DeleteResult><Error><Key></Key><Code>c</Code></Error></DeleteResult>"[..],
            vec![Err(ListError::Malformed)],
        ),
        (
            b"<DeleteResult><Error><Code>c</Code></Error><Error><Key>k</Key><Code>c</Code></Error></DeleteResult>",
            vec![Err(ListError::Malformed)],
        ),
        (
            b"<DeleteResult><Error><Key>k</Key><Code></Code></Error></DeleteResult>",
            vec![Ok(Failure {
                key_raw: b"k",
                code: b"",
            })],
        ),
    ] {
        assert_eq!(
            Failures::parse(doc).unwrap().collect::<Vec<_>>(),
            expected,
            "{}",
            String::from_utf8_lossy(doc)
        );
    }
}
