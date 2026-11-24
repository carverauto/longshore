// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn clearing_wipes_the_key_schedule_not_just_the_flag() {
    // The providers keyed from raw bytes; rustls's keys come from a handshake.
    let providers = [Provider::AwsLc, Provider::Ring]
        .into_iter()
        .filter(|p| p.compiled());
    for provider in providers {
        for (suite, key) in [
            (Suite::Aes128Gcm, &[0x5a; 32][..16]),
            (Suite::Aes256Gcm, &[0x5a; 32][..]),
        ] {
            let mut d = Direction::new();
            assert!(!d.holds_key_material());
            d.set_keys_with(provider, suite, key, &[7; 12], 0).unwrap();
            assert!(d.holds_key_material());
            d.clear();
            assert!(
                !d.holds_key_material(),
                "{provider:?} {suite:?}: key material left behind"
            );
            assert_eq!(d.seal(23, b"x", &mut [0u8; 64]), Err(RecordError::BadKey));
        }
    }
}
