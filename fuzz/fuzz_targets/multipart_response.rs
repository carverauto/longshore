// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

#![no_main]
//! The multipart response readers face whatever the store sends. They must
//! never panic, and what they return must lie inside the input. A manifest
//! written from any tags must fit the space it reports.

use libfuzzer_sys::fuzz_target;

// Compiled straight from its source file; see fuzz/Cargo.toml.
#[allow(dead_code)]
#[path = "../../crates/longshore/src/store/multipart.rs"]
mod multipart;
use multipart::{error_code_in, upload_id_in, write_manifest, PartTag, UploadId};

fuzz_target!(|data: &[u8]| {
    let inside = |part: &[u8]| {
        let (a, b) = (data.as_ptr() as usize, part.as_ptr() as usize);
        part.is_empty() || (b >= a && b + part.len() <= a + data.len())
    };
    if let Some(id) = upload_id_in(data) {
        assert!(!id.is_empty() && inside(id));
        let _ = UploadId::new(id);
    }
    if let Some(code) = error_code_in(data) {
        assert!(inside(code));
    }
    // Any bytes that fit a tag can be written into a manifest and never
    // spill past the length reported.
    if let Some(tag) = PartTag::new(&data[..data.len().min(64)]) {
        let mut out = [0xA5u8; 1024];
        if let Some(n) = write_manifest(&mut out[..900], &[(1, tag), (2, tag)]) {
            assert!(n <= 900);
            assert!(out[900..].iter().all(|b| *b == 0xA5));
        }
    }
});
