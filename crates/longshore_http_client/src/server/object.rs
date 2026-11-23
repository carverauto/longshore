// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! An object slot in the store's table.

use super::consts::{DESCRIBING_MAX, TARGET_MAX};
use super::identity;

/// An object slot in the store's table, with room for one buffer of data.
pub(super) struct Object {
    pub(super) target: [u8; TARGET_MAX],
    pub(super) target_len: usize,
    pub(super) len: u32,
    pub(super) data: Box<[u8]>,
    pub(super) used: bool,
    pub(super) checksum: Option<[u8; 12]>,
    /// Content type and metadata headers it was stored with, as raw lines.
    pub(super) describing: [u8; DESCRIBING_MAX],
    pub(super) describing_len: usize,
    /// Its version, encryption and SHA-256.
    pub(super) kept: identity::Kept,
}

impl Object {
    /// An unused object slot, with room for `buffer_size` bytes of data.
    pub(super) fn empty(buffer_size: u32) -> Object {
        Object {
            target: [0; TARGET_MAX],
            target_len: 0,
            len: 0,
            data: vec![0u8; buffer_size as usize].into_boxed_slice(),
            used: false,
            checksum: None,
            describing: [0; DESCRIBING_MAX],
            describing_len: 0,
            kept: Default::default(),
        }
    }
}
