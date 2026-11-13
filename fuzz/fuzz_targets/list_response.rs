// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! The list-response parser faces whatever the store sends. It must never
//! panic, and unescaping must never produce more bytes than it was given.

use libfuzzer_sys::fuzz_target;

// Compiled straight from its source file; see fuzz/Cargo.toml.
#[allow(dead_code)]
#[path = "../../crates/longshore/src/store/list.rs"]
mod list;
use list::{unescape, Listing};

fuzz_target!(|data: &[u8]| {
    if let Ok(listing) = Listing::parse(data) {
        for e in listing.entries().flatten() {
            assert!(!e.key_raw.is_empty());
            let mut out = vec![0u8; e.key_raw.len()];
            if let Some(n) = unescape(e.key_raw, &mut out) {
                assert!(n <= e.key_raw.len());
            }
        }
    }
    let mut out = vec![0u8; data.len()];
    let _ = unescape(data, &mut out);
});
