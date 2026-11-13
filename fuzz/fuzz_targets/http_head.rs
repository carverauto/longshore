// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! HTTP head parsers must never panic or point outside their input, and a
//! verdict on a prefix must not change when more bytes arrive.

use libfuzzer_sys::fuzz_target;
use longshore::http::head::{parse_request, parse_response};

fuzz_target!(|data: &[u8]| {
    let limit = 4096;
    let whole = parse_response(data, limit);
    if let Ok(Some(h)) = whole {
        assert!(h.fields.head_len <= data.len());
        let _ = h.fields.header(data, "content-length");
    }
    // Split point taken from the input itself.
    let cut = data.first().map_or(0, |b| *b as usize * data.len() / 256);
    match parse_response(&data[..cut], limit) {
        Ok(None) => {}
        decided => assert_eq!(decided, whole),
    }
    if let Ok(Some(h)) = parse_request(data, limit) {
        assert!(h.fields.head_len <= data.len());
        let _ = (h.method(data), h.target(data));
    }
});
