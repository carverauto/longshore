// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Record layer, the refusals: a destination one byte short, records that
//! are framed correctly and say nothing, unkeyed directions, and a content
//! type that cannot be carried.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_tls::record::*;

const KEY: [u8; 16] = [
    0x17, 0x42, 0x2d, 0xda, 0x59, 0x6e, 0xd5, 0xd9, 0xac, 0xd8, 0x90, 0xe3, 0xc6, 0x3f, 0x50, 0x51,
];
const IV: [u8; 12] = [
    0x5b, 0x78, 0x92, 0x3d, 0xee, 0x08, 0x57, 0x90, 0x33, 0xe5, 0x23, 0xd9,
];

fn direction(seq: u64) -> Direction {
    let mut d = Direction::new();
    d.set_keys(Suite::Aes128Gcm, &KEY, &IV, seq).unwrap();
    d
}

fn sealed(content_type: u8, content: &[u8], seq: u64) -> Vec<u8> {
    let mut rec = vec![0u8; content.len() + OVERHEAD];
    let n = direction(seq)
        .seal(content_type, content, &mut rec)
        .unwrap();
    assert_eq!(n, rec.len());
    rec
}

/// A record's last non-zero byte is its content type, so a type of zero
/// cannot be told from padding: the receiver would take the content's last
/// non-zero byte for the type and lose it from the content.
#[test]
fn a_content_type_of_zero_is_not_sealed() {
    let mut tx = direction(0);
    let mut rec = [0xeeu8; 64];
    assert_eq!(tx.seal(0, b"ab", &mut rec), Err(RecordError::NoContentType));
    assert_eq!(tx.sequence(), 0, "nothing was sealed");
    assert!(rec.iter().all(|b| *b == 0xee), "nothing was written");
    // The keys are still good for a real record.
    let n = tx.seal(CONTENT_APPLICATION_DATA, b"ab", &mut rec).unwrap();
    let mut out = [0u8; 8];
    assert_eq!(
        direction(0).open(&rec[..n], &mut out),
        Ok((CONTENT_APPLICATION_DATA, 2))
    );
    assert_eq!(&out[..2], b"ab");
}

/// `open` decrypts into the destination through a raw pointer; the check in
/// front of it is exact.
#[hegel::test]
fn a_destination_one_byte_short_is_refused_and_an_exact_one_is_enough(tc: TestCase) {
    let content = tc.draw(gs::binary().max_size(300));
    let rec = sealed(CONTENT_APPLICATION_DATA, &content, 5);
    // Content plus its type byte.
    let need = content.len() + 1;
    let mut rx = direction(5);
    let mut short = vec![0xeeu8; need + 4];
    assert_eq!(
        rx.open(&rec, &mut short[..need - 1]),
        Err(RecordError::DestinationTooSmall)
    );
    assert!(short.iter().all(|b| *b == 0xee), "nothing was written");
    assert_eq!(rx.sequence(), 5, "a refusal for size opens nothing");
    let mut exact = vec![0xeeu8; need + 4];
    assert_eq!(
        rx.open(&rec, &mut exact[..need]),
        Ok((CONTENT_APPLICATION_DATA, content.len()))
    );
    assert_eq!(&exact[..content.len()], &content[..]);
    assert!(
        exact[need..].iter().all(|b| *b == 0xee),
        "wrote past the end"
    );
}

#[test]
fn the_rfc_8448_record_needs_fifty_one_bytes() {
    // 50 bytes of content and the type.
    let content: Vec<u8> = (0..50u8).collect();
    let rec = sealed(CONTENT_APPLICATION_DATA, &content, 0);
    assert_eq!(
        direction(0).open(&rec, &mut [0u8; 50]),
        Err(RecordError::DestinationTooSmall)
    );
    assert_eq!(
        direction(0).open(&rec, &mut [0u8; 51]),
        Ok((CONTENT_APPLICATION_DATA, 50))
    );
}

/// Bytes framed as a record of the right length, with anything inside, do
/// not authenticate, whichever way they are opened.
#[hegel::test]
fn well_framed_records_of_arbitrary_bytes_do_not_authenticate(tc: TestCase) {
    let n = tc.draw(
        gs::integers::<usize>()
            .min_value(1 + TAG_LEN)
            .max_value(300),
    );
    let body = tc.draw(gs::binary().min_size(n).max_size(n));
    let mut rec = vec![CONTENT_APPLICATION_DATA, 3, 3, (n >> 8) as u8, n as u8];
    rec.extend_from_slice(&body);
    assert_eq!(record_len(&rec), Ok(Some(rec.len())));
    let mut out = [0u8; 400];
    assert_eq!(
        direction(0).open(&rec, &mut out),
        Err(RecordError::BadRecord)
    );
    assert_eq!(
        direction(0).open_in_place(&mut rec.clone()),
        Err(RecordError::BadRecord)
    );

    // One byte more or fewer than the header declares is not a record.
    let mut long = rec.clone();
    long.push(0);
    for wrong in [&rec[..rec.len() - 1], &long[..]] {
        assert_eq!(
            direction(0).open(wrong, &mut out),
            Err(RecordError::BadLength)
        );
        assert_eq!(
            direction(0).open_in_place(&mut wrong.to_vec()),
            Err(RecordError::BadLength)
        );
    }
}

/// Any single flipped bit is caught when the record is opened in place too.
#[test]
fn a_modified_record_is_refused_in_place() {
    let rec = sealed(CONTENT_APPLICATION_DATA, b"sixteen byte msg", 3);
    for bit in 0..rec.len() * 8 {
        let mut bad = rec.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        let r = direction(3).open_in_place(&mut bad);
        // The header's own bits are refused before the cipher sees them.
        let want = match bit / 8 {
            0..=2 => RecordError::BadHeader,
            3..=4 => RecordError::BadLength,
            _ => RecordError::BadRecord,
        };
        assert_eq!(r, Err(want), "bit {bit}");
    }
    assert_eq!(
        direction(4).open_in_place(&mut rec.clone()),
        Err(RecordError::BadRecord),
        "the right record at the wrong place in the stream"
    );
    let mut good = rec.clone();
    assert_eq!(
        direction(3).open_in_place(&mut good),
        Ok((CONTENT_APPLICATION_DATA, 16))
    );
    assert_eq!(&good[HEADER_LEN..HEADER_LEN + 16], b"sixteen byte msg");
}

#[test]
fn an_unkeyed_direction_opens_nothing() {
    let mut rec = sealed(CONTENT_APPLICATION_DATA, b"x", 0);
    let mut d = Direction::new();
    assert_eq!(d.open(&rec, &mut [0u8; 8]), Err(RecordError::BadKey));
    assert_eq!(d.open_in_place(&mut rec), Err(RecordError::BadKey));
    // And neither does one whose keys were cleared.
    let mut cleared = direction(0);
    cleared.clear();
    assert_eq!(cleared.open(&rec, &mut [0u8; 8]), Err(RecordError::BadKey));
    assert_eq!(cleared.open_in_place(&mut rec), Err(RecordError::BadKey));
    assert_eq!(Direction::default().sequence(), 0);
}

/// Keys installed over earlier keys replace them, with their own counter.
#[test]
fn keying_again_replaces_the_keys_and_the_sequence() {
    let rec = sealed(CONTENT_APPLICATION_DATA, b"second keys", 9);
    let mut d = Direction::new();
    d.set_keys(Suite::Aes256Gcm, &[7; 32], &[9; 12], 1000)
        .unwrap();
    let mut out = [0u8; 16];
    assert_eq!(d.open(&rec, &mut out), Err(RecordError::BadRecord));
    d.set_keys(Suite::Aes128Gcm, &KEY, &IV, 9).unwrap();
    assert_eq!(d.sequence(), 9);
    assert_eq!(d.open(&rec, &mut out), Ok((CONTENT_APPLICATION_DATA, 11)));
    assert_eq!((&out[..11], d.sequence()), (&b"second keys"[..], 10));
}
