// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Stand-ins for the providers whose features are off: they take no space,
//! refuse every key and so are never asked to seal or open.

#[cfg(not(feature = "aws-lc"))]
pub(super) struct AwsLc;

#[cfg(not(feature = "aws-lc"))]
impl AwsLc {
    pub(super) fn new() -> AwsLc {
        AwsLc
    }
    pub(super) fn init(&mut self, _: super::Suite, _: &[u8]) -> bool {
        false
    }
    pub(super) fn seal(
        &self,
        _: &[u8; 12],
        _: [u8; super::HEADER_LEN],
        _: &[u8],
        _: u8,
        _: &mut [u8],
        _: &mut [u8],
    ) -> bool {
        false
    }
    pub(super) fn open(&self, _: &[u8; 12], _: &[u8], _: &[u8], _: &[u8], _: &mut [u8]) -> bool {
        false
    }
    pub(super) fn open_in_place(
        &self,
        _: &[u8; 12],
        _: [u8; super::HEADER_LEN],
        _: &mut [u8],
        _: &[u8],
    ) -> bool {
        false
    }
    pub(super) fn wipe(&mut self) {}
    #[cfg(test)]
    pub(super) fn holds_key_material(&self) -> bool {
        false
    }
}

#[cfg(not(feature = "ring"))]
pub(super) struct Ring;

#[cfg(not(feature = "ring"))]
impl Ring {
    pub(super) fn new() -> Ring {
        Ring
    }
    pub(super) fn init(&mut self, _: super::Suite, _: &[u8]) -> bool {
        false
    }
    pub(super) fn seal(
        &self,
        _: &[u8; 12],
        _: [u8; super::HEADER_LEN],
        _: &[u8],
        _: u8,
        _: &mut [u8],
    ) -> bool {
        false
    }
    pub(super) fn open(&self, _: &[u8; 12], _: &[u8], _: &[u8], _: &[u8], _: &mut [u8]) -> bool {
        false
    }
    pub(super) fn open_in_place(&self, _: &[u8; 12], _: &[u8], _: &mut [u8], _: &[u8]) -> bool {
        false
    }
    pub(super) fn wipe(&mut self) {}
    #[cfg(test)]
    pub(super) fn holds_key_material(&self) -> bool {
        false
    }
}

#[cfg(not(feature = "rustls"))]
pub(super) struct Rustls;

#[cfg(not(feature = "rustls"))]
impl Rustls {
    pub(super) fn new() -> Rustls {
        Rustls
    }
    #[cfg(test)]
    pub(super) fn is_keyed(&self) -> bool {
        false
    }
    pub(super) fn seal(&mut self, _: u64, _: &[u8], _: u8, _: &mut [u8], _: usize) -> bool {
        false
    }
    pub(super) fn open(&mut self, _: u64, _: &mut [u8]) -> Option<(u8, usize)> {
        None
    }
    pub(super) fn wipe(&mut self) {}
}
