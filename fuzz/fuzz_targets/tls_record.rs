// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! The TLS record layer must reject anything that is not an authentic
//! record, without panicking, and must frame consistently: a length decided
//! from a prefix does not change when more bytes arrive. Every provider
//! must refuse the same garbage, seal the same bytes, and open the others'.

use libfuzzer_sys::fuzz_target;
use longshore_tls::record::{
    record_len, Direction, Provider, Role, Suite, CONTENT_APPLICATION_DATA, OVERHEAD,
};

const KEY: [u8; 32] = [3; 32];
const IV: [u8; 12] = [5; 12];

/// AWS-LC, ring and rustls, keyed alike for AES-256-GCM, starting at `seq`.
fn directions(seq: u64, role: Role) -> [Direction; 3] {
    let mut out = [Direction::new(), Direction::new(), Direction::new()];
    out[0].set_keys_with(Provider::AwsLc, Suite::Aes256Gcm, &KEY, &IV, seq).unwrap();
    out[1].set_keys_with(Provider::Ring, Suite::Aes256Gcm, &KEY, &IV, seq).unwrap();
    out[2].set_rustls_keys(Suite::Aes256Gcm, KEY.into(), &IV, seq, role).unwrap();
    out
}

fuzz_target!(|data: &[u8]| {
    let whole = record_len(data);
    let cut = data.first().map_or(0, |b| *b as usize * data.len() / 256);
    match record_len(&data[..cut]) {
        Ok(None) => {}
        decided => assert_eq!(decided, whole),
    }

    // Without the key's cooperation nothing the fuzzer writes can authenticate.
    let mut out = vec![0u8; data.len() + 1];
    for mut rx in directions(0, Role::Open) {
        assert!(rx.open(data, &mut out).is_err());
        let mut copy = data.to_vec();
        assert!(rx.open_in_place(&mut copy).is_err());
    }

    // What each provider seals is the same record, whatever the content, and
    // opens with every provider.
    let content = &data[..data.len().min(16384)];
    let mut records = Vec::new();
    for mut tx in directions(41, Role::Seal) {
        let mut rec = vec![0u8; content.len() + OVERHEAD];
        let n = tx.seal(CONTENT_APPLICATION_DATA, content, &mut rec).unwrap();
        rec.truncate(n);
        records.push(rec);
    }
    assert!(records.iter().all(|r| *r == records[0]));
    for mut rx in directions(41, Role::Open) {
        let mut dst = vec![0u8; rx.open_room(records[0].len())];
        assert_eq!(rx.open(&records[0], &mut dst), Ok((CONTENT_APPLICATION_DATA, content.len())));
        assert_eq!(&dst[..content.len()], content);
    }
});
