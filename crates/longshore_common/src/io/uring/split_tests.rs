// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::split::{attr_of, split_in, Split};

/// A generic netlink message of type `kind` with `attrs`, each `(type,
/// value)`, laid out as the kernel lays a reply out.
fn message(kind: u16, attrs: &[(u16, &[u8])]) -> Vec<u8> {
    let mut m = vec![0u8; 20];
    for (t, v) in attrs {
        let len = 4 + v.len();
        m.extend_from_slice(&(len as u16).to_ne_bytes());
        m.extend_from_slice(&t.to_ne_bytes());
        m.extend_from_slice(v);
        m.resize(m.len().next_multiple_of(4), 0);
    }
    let total = m.len() as u32;
    m[..4].copy_from_slice(&total.to_ne_bytes());
    m[4..6].copy_from_slice(&kind.to_ne_bytes());
    m
}

const REPLY: u16 = 0x20;

#[test]
fn a_reply_names_the_split_after_attributes_of_other_lengths() {
    let header = [8u8, 0, 1, 0x80, 7, 0, 0, 0];
    for (value, split) in [(1u8, Split::Off), (2, Split::On), (0, Split::Unknown)] {
        let m = message(
            REPLY,
            &[
                (1 | 0x8000, &header),
                (6, &1024u32.to_ne_bytes()),
                (10, &[]),
                (11, &[value]),
            ],
        );
        assert_eq!(split_in(&m), Some(split));
    }
}

#[test]
fn a_reply_without_the_attribute_says_unknown() {
    let m = message(REPLY, &[(6, &1024u32.to_ne_bytes())]);
    assert_eq!(split_in(&m), Some(Split::Unknown));
}

#[test]
fn an_error_is_no_answer() {
    let m = message(2, &[(11, &[2])]);
    assert_eq!(split_in(&m), None);
}

#[test]
fn a_cut_message_never_reads_past_its_end() {
    let m = message(REPLY, &[(6, &1024u32.to_ne_bytes()), (11, &[2])]);
    for end in 0..m.len() {
        // A cut message is read without a panic, and an attribute is found
        // only whole: a cut in its padding leaves its value whole.
        let found = attr_of(&m[..end], 11);
        assert!(
            found.is_none() || found == Some(&[2][..]),
            "{found:?} in {end} bytes"
        );
        assert!(
            found.is_none() || end >= m.len() - 3,
            "found in {end} of {} bytes",
            m.len()
        );
    }
    // An attribute whose length runs past the message is not taken.
    let mut lying = message(REPLY, &[(11, &[2])]);
    lying[20] = 200;
    assert_eq!(attr_of(&lying, 11), None);
}

#[test]
fn the_family_id_is_found_behind_its_name() {
    let m = message(REPLY, &[(2, b"ethtool\0"), (1, &21u16.to_ne_bytes())]);
    assert_eq!(attr_of(&m, 1), Some(&21u16.to_ne_bytes()[..]));
}
