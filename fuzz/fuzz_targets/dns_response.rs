// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! The DNS response parser must never panic or read out of bounds, whatever
//! the network hands it. The first two bytes pick the expected query id so the
//! fuzzer can get past the id check.

use libfuzzer_sys::fuzz_target;
// Compiled straight from its source file; see fuzz/Cargo.toml.
#[allow(dead_code)]
#[path = "../../crates/longshore/src/dns/codec.rs"]
mod codec;
use codec::parse_response;

fuzz_target!(|data: &[u8]| {
    let id = if data.len() >= 2 { u16::from_be_bytes([data[0], data[1]]) } else { 0 };
    for name in ["s3.example.com", "a.b", "bucket.us-east-1.linodeobjects.com"] {
        if let Ok(a) = parse_response(data, id, name) {
            assert!(a.len >= 1 && a.len <= a.addrs.len());
        }
    }
});
