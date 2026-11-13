// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! The TLS record layer must reject anything that is not an authentic
//! record, without panicking, and must frame consistently: a length decided
//! from a prefix does not change when more bytes arrive.

use libfuzzer_sys::fuzz_target;
use longshore::tls::record::{record_len, Direction, Suite, CONTENT_APPLICATION_DATA, OVERHEAD};

fuzz_target!(|data: &[u8]| {
    let whole = record_len(data);
    let cut = data.first().map_or(0, |b| *b as usize * data.len() / 256);
    match record_len(&data[..cut]) {
        Ok(None) => {}
        decided => assert_eq!(decided, whole),
    }

    let mut rx = Direction::new();
    rx.set_keys(Suite::Aes128Gcm, &[7; 16], &[9; 12], 0).unwrap();
    let mut out = vec![0u8; data.len() + 1];
    // Without the key's cooperation nothing the fuzzer writes can authenticate.
    assert!(rx.open(data, &mut out).is_err());
    let mut copy = data.to_vec();
    assert!(rx.open_in_place(&mut copy).is_err());

    // What we seal ourselves always opens, whatever the content.
    let content = &data[..data.len().min(16384)];
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    tx.set_keys(Suite::Aes256Gcm, &[3; 32], &[5; 12], 41).unwrap();
    rx.set_keys(Suite::Aes256Gcm, &[3; 32], &[5; 12], 41).unwrap();
    let mut rec = vec![0u8; content.len() + OVERHEAD];
    let n = tx.seal(CONTENT_APPLICATION_DATA, content, &mut rec).unwrap();
    assert_eq!(rx.open(&rec[..n], &mut out[..content.len() + 1]), Ok((CONTENT_APPLICATION_DATA, content.len())));
    assert_eq!(&out[..content.len()], content);
});
