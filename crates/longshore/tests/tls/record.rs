// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Record layer: RFC 8448 vectors, an independent implementation, tamper
//! detection, properties, and an allocation check.

use hegel::generators as gs;
use hegel::TestCase;
use longshore::io::alloc_guard::{sealed, Guard};
use longshore::tls::record::*;

#[global_allocator]
static ALLOC: Guard = Guard;

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

// RFC 8448 section 3, "{client} derive write traffic keys for application data".
const KEY: &str = "17 42 2d da 59 6e d5 d9 ac d8 90 e3 c6 3f 50 51";
const IV: &str = "5b 78 92 3d ee 08 57 90 33 e5 23 d9";
// "{client} send application_data record", sequence number 0.
const RECORD_0: &str = "17 03 03 00 43 a2 3f 70 54 b6 2c 94 d0 af fa fe 82 28 ba 55 cb ef ac ea 42 f9 14 aa 66 bc ab 3f 2b
    98 19 a8 a5 b4 6b 39 5b d5 4a 9a 20 44 1e 2b 62 97 4e 1f 5a 62 92 a2 97 70 14 bd 1e 3d ea e6 3a ee bb 21 69 49 15 e4";
// "{client} send alert record" (close_notify), sequence number 1.
const RECORD_1: &str = "17 03 03 00 13 c9 87 27 60 65 56 66 b7 4d 7f f1 15 3e fd 6d b6 d0 b0 e3";

fn direction(key: &[u8], iv: &[u8], seq: u64) -> Direction {
    let mut d = Direction::new();
    let suite = if key.len() == 16 {
        Suite::Aes128Gcm
    } else {
        Suite::Aes256Gcm
    };
    d.set_keys(suite, key, iv.try_into().unwrap(), seq).unwrap();
    d
}

#[test]
fn seals_the_rfc_8448_records_byte_for_byte() {
    let mut d = direction(&hex(KEY), &hex(IV), 0);
    let payload: Vec<u8> = (0..50u8).collect();
    let mut out = [0u8; 128];
    let n = d
        .seal(CONTENT_APPLICATION_DATA, &payload, &mut out)
        .unwrap();
    assert_eq!(
        &out[..n],
        &hex(RECORD_0)[..],
        "application data record, sequence 0"
    );
    let n = d.seal(CONTENT_ALERT, &[1, 0], &mut out).unwrap();
    assert_eq!(&out[..n], &hex(RECORD_1)[..], "alert record, sequence 1");
    assert_eq!(d.sequence(), 2);
}

#[test]
fn opens_the_rfc_8448_records() {
    let mut d = direction(&hex(KEY), &hex(IV), 0);
    let mut out = [0u8; 128];
    assert_eq!(
        d.open(&hex(RECORD_0), &mut out),
        Ok((CONTENT_APPLICATION_DATA, 50))
    );
    assert_eq!(&out[..50], &(0..50u8).collect::<Vec<_>>()[..]);
    assert_eq!(d.open(&hex(RECORD_1), &mut out), Ok((CONTENT_ALERT, 2)));
    assert_eq!(&out[..2], &[1, 0], "close_notify");
}

#[test]
fn rejects_modified_reordered_and_misdirected_records() {
    let (key, iv, rec) = (hex(KEY), hex(IV), hex(RECORD_0));
    let mut out = [0u8; 128];
    // Any single flipped bit, anywhere in the record.
    for bit in 0..rec.len() * 8 {
        let mut bad = rec.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        let r = direction(&key, &iv, 0).open(&bad, &mut out);
        assert!(
            r.is_err(),
            "accepted a record with bit {bit} flipped: {r:?}"
        );
    }
    // The right record at the wrong position in the stream.
    assert_eq!(
        direction(&key, &iv, 1).open(&rec, &mut out),
        Err(RecordError::BadRecord)
    );
    // The wrong key, or the wrong IV.
    let mut k2 = key.clone();
    k2[0] ^= 1;
    assert_eq!(
        direction(&k2, &iv, 0).open(&rec, &mut out),
        Err(RecordError::BadRecord)
    );
    let mut iv2 = iv.clone();
    iv2[11] ^= 1;
    assert_eq!(
        direction(&key, &iv2, 0).open(&rec, &mut out),
        Err(RecordError::BadRecord)
    );
    // Truncated or extended.
    assert_eq!(
        direction(&key, &iv, 0).open(&rec[..rec.len() - 1], &mut out),
        Err(RecordError::BadLength)
    );
    let mut long = rec.clone();
    long.push(0);
    assert_eq!(
        direction(&key, &iv, 0).open(&long, &mut out),
        Err(RecordError::BadLength)
    );
}

#[test]
fn record_length_framing() {
    assert_eq!(record_len(&[]), Ok(None));
    assert_eq!(record_len(&[23, 3, 3, 0]), Ok(None));
    assert_eq!(record_len(&hex(RECORD_0)[..5]), Ok(Some(72)));
    assert_eq!(
        record_len(&[22]),
        Err(RecordError::BadHeader),
        "wrong type is rejected on its first byte"
    );
    assert_eq!(
        record_len(&[23, 3, 1]),
        Err(RecordError::BadHeader),
        "wrong version"
    );
    assert_eq!(
        record_len(&[23, 3, 3, 0, 16]),
        Err(RecordError::BadLength),
        "no room for a content type"
    );
    assert_eq!(
        record_len(&[23, 3, 3, 0, 17]),
        Ok(Some(22)),
        "smallest record: empty content"
    );
    assert_eq!(
        record_len(&[23, 3, 3, 0x41, 0x00]),
        Ok(Some(5 + 16640)),
        "largest allowed"
    );
    assert_eq!(
        record_len(&[23, 3, 3, 0x41, 0x01]),
        Err(RecordError::BadLength)
    );
}

#[test]
fn padding_is_stripped_and_all_padding_is_rejected() {
    // Build padded records with an independent AES-GCM (ring).
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM};
    let (key, iv) = (hex(KEY), hex(IV));
    let seal_raw = |inner: &[u8]| {
        let k = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &key).unwrap());
        let len = (inner.len() + 16) as u16;
        let header = [23, 3, 3, (len >> 8) as u8, len as u8];
        let mut body = inner.to_vec();
        let tag = k
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(iv.clone().try_into().unwrap()),
                Aad::from(header),
                &mut body,
            )
            .unwrap();
        [&header[..], &body, tag.as_ref()].concat()
    };
    let mut out = [0u8; 128];
    let padded = seal_raw(&[b'h', b'i', CONTENT_APPLICATION_DATA, 0, 0, 0, 0]);
    assert_eq!(
        direction(&key, &iv, 0).open(&padded, &mut out),
        Ok((CONTENT_APPLICATION_DATA, 2))
    );
    assert_eq!(&out[..2], b"hi");
    let empty = seal_raw(&[CONTENT_HANDSHAKE]);
    assert_eq!(
        direction(&key, &iv, 0).open(&empty, &mut out),
        Ok((CONTENT_HANDSHAKE, 0))
    );
    let all_padding = seal_raw(&[0, 0, 0, 0]);
    assert_eq!(
        direction(&key, &iv, 0).open(&all_padding, &mut out),
        Err(RecordError::NoContentType)
    );

    // Content, type and padding together may be at most 2^14 + 1 bytes
    // (RFC 8446, section 5.4), however little of it is content.
    let mut big = vec![0u8; FRAGMENT_MAX + 64];
    let mut inner = vec![0u8; FRAGMENT_MAX + 1];
    inner[..3].copy_from_slice(&[b'h', b'i', CONTENT_APPLICATION_DATA]);
    let at_limit = seal_raw(&inner);
    assert_eq!(
        direction(&key, &iv, 0).open(&at_limit, &mut big),
        Ok((CONTENT_APPLICATION_DATA, 2))
    );
    assert_eq!(
        direction(&key, &iv, 0).open_in_place(&mut at_limit.clone()),
        Ok((CONTENT_APPLICATION_DATA, 2))
    );
    inner.push(0);
    let over = seal_raw(&inner);
    assert_eq!(
        direction(&key, &iv, 0).open(&over, &mut big),
        Err(RecordError::BadLength)
    );
    assert_eq!(
        direction(&key, &iv, 0).open_in_place(&mut over.clone()),
        Err(RecordError::BadLength)
    );
}

#[test]
fn limits_and_unkeyed_use() {
    let mut out = vec![0u8; RECORD_MAX + 8];
    let mut d = Direction::new();
    assert_eq!(
        d.seal(23, b"x", &mut out),
        Err(RecordError::BadKey),
        "unkeyed"
    );
    assert_eq!(
        d.set_keys(Suite::Aes128Gcm, &[0; 32], &[0; 12], 0),
        Err(RecordError::BadKey),
        "key length must match the suite"
    );
    d.set_keys(Suite::Aes256Gcm, &[7; 32], &[9; 12], 0).unwrap();
    let big = vec![0x5a; FRAGMENT_MAX];
    assert_eq!(d.seal(23, &big, &mut out), Ok(RECORD_MAX));
    assert_eq!(
        d.seal(23, &vec![0; FRAGMENT_MAX + 1], &mut out),
        Err(RecordError::BadLength)
    );
    assert_eq!(
        d.seal(23, &big, &mut out[..RECORD_MAX - 1]),
        Err(RecordError::DestinationTooSmall)
    );
    // The keys stop at the record limit, in both directions, and stay stopped.
    let mut last = direction(&hex(KEY), &hex(IV), KEY_RECORDS_MAX - 1);
    let n = last
        .seal(23, b"x", &mut out)
        .expect("the last record the keys may protect");
    let mut record = out[..n].to_vec();
    assert_eq!(
        last.seal(23, b"x", &mut out),
        Err(RecordError::SequenceExhausted)
    );
    assert_eq!(
        last.seal(23, b"x", &mut out),
        Err(RecordError::SequenceExhausted)
    );
    let mut rx = direction(&hex(KEY), &hex(IV), KEY_RECORDS_MAX - 1);
    assert_eq!(rx.open(&record, &mut [0u8; 8]), Ok((23, 1)));
    assert_eq!(
        rx.open(&record, &mut [0u8; 8]),
        Err(RecordError::SequenceExhausted)
    );
    assert_eq!(
        rx.open_in_place(&mut record),
        Err(RecordError::SequenceExhausted)
    );
    d.clear();
    assert_eq!(
        d.seal(23, b"x", &mut out),
        Err(RecordError::BadKey),
        "cleared keys are gone"
    );
}

/// Whatever is sealed opens to the same content and type, for both suites,
/// and the sealed record equals what an independent AES-GCM produces.
#[hegel::test]
fn seal_then_open_round_trips_and_matches_an_independent_cipher(tc: TestCase) {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM, AES_256_GCM};
    let wide = tc.draw(gs::booleans());
    let key = tc.draw(
        gs::binary()
            .min_size(if wide { 32 } else { 16 })
            .max_size(if wide { 32 } else { 16 }),
    );
    let iv = tc.draw(gs::binary().min_size(12).max_size(12));
    let seq = tc.draw(gs::integers::<u64>().max_value(KEY_RECORDS_MAX - 2));
    let content = tc.draw(gs::binary().max_size(600));
    let ctype = tc.draw(gs::sampled_from(vec![
        CONTENT_ALERT,
        CONTENT_HANDSHAKE,
        CONTENT_APPLICATION_DATA,
    ]));

    let mut rec = vec![0u8; content.len() + OVERHEAD];
    let n = direction(&key, &iv, seq)
        .seal(ctype, &content, &mut rec)
        .unwrap();
    assert_eq!((n, record_len(&rec)), (rec.len(), Ok(Some(rec.len()))));

    // Independent construction of the same record.
    let k = LessSafeKey::new(
        UnboundKey::new(if wide { &AES_256_GCM } else { &AES_128_GCM }, &key).unwrap(),
    );
    let mut nonce: [u8; 12] = iv.clone().try_into().unwrap();
    for (b, s) in nonce[4..].iter_mut().zip(seq.to_be_bytes()) {
        *b ^= s;
    }
    let mut body = [&content[..], &[ctype]].concat();
    let header: [u8; 5] = rec[..5].try_into().unwrap();
    let tag = k
        .seal_in_place_separate_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(header),
            &mut body,
        )
        .unwrap();
    assert_eq!(
        &rec[5..],
        &[&body[..], tag.as_ref()].concat()[..],
        "record differs from an independent AES-GCM"
    );

    let mut out = vec![0u8; content.len() + 1];
    assert_eq!(
        direction(&key, &iv, seq).open(&rec, &mut out),
        Ok((ctype, content.len()))
    );
    assert_eq!(&out[..content.len()], &content[..]);
}

/// Arbitrary bytes presented as a record are rejected, never accepted.
#[hegel::test]
fn arbitrary_records_are_rejected(tc: TestCase) {
    let mut d = direction(&hex(KEY), &hex(IV), 0);
    let junk = tc.draw(gs::binary().max_size(300));
    let mut out = [0u8; 400];
    assert!(d.open(&junk, &mut out).is_err());
    let _ = record_len(&junk);
}

#[test]
fn sealing_and_opening_do_not_allocate() {
    let (key, iv) = (hex(KEY), hex(IV));
    let content = vec![0xa5u8; FRAGMENT_MAX];
    let mut rec = vec![0u8; RECORD_MAX];
    let mut out = vec![0u8; FRAGMENT_MAX + 1];
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    let key16: [u8; 16] = key.clone().try_into().unwrap();
    let iv12: [u8; 12] = iv.clone().try_into().unwrap();
    // Keying happens at connection setup, after initialization, so it is sealed too.
    let ok = sealed(|| {
        tx.set_keys(Suite::Aes128Gcm, &key16, &iv12, 0).unwrap();
        rx.set_keys(Suite::Aes128Gcm, &key16, &iv12, 0).unwrap();
        let mut ok = true;
        for _ in 0..64 {
            let n = tx
                .seal(CONTENT_APPLICATION_DATA, &content, &mut rec)
                .unwrap();
            ok &= rx.open(&rec[..n], &mut out) == Ok((CONTENT_APPLICATION_DATA, FRAGMENT_MAX));
        }
        ok
    });
    assert!(ok && out[..FRAGMENT_MAX] == content[..]);
}
