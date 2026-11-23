// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters for the [`DiskFaultCounters`].

use super::DiskFaultCounters;

impl DiskFaultCounters {
    /// Faults of kind `short_write` injected so far.
    #[inline]
    pub const fn short_write(self) -> u64 {
        self.short_write
    }

    /// Faults of kind `torn_reported` injected so far.
    #[inline]
    pub const fn torn_reported(self) -> u64 {
        self.torn_reported
    }

    /// Faults of kind `torn_silent` injected so far.
    #[inline]
    pub const fn torn_silent(self) -> u64 {
        self.torn_silent
    }

    /// Faults of kind `lost_reported` injected so far.
    #[inline]
    pub const fn lost_reported(self) -> u64 {
        self.lost_reported
    }

    /// Faults of kind `lost_silent` injected so far.
    #[inline]
    pub const fn lost_silent(self) -> u64 {
        self.lost_silent
    }

    /// Faults of kind `misdirected_write` injected so far.
    #[inline]
    pub const fn misdirected_write(self) -> u64 {
        self.misdirected_write
    }

    /// Faults of kind `read_error` injected so far.
    #[inline]
    pub const fn read_error(self) -> u64 {
        self.read_error
    }

    /// Faults of kind `bit_flip` injected so far.
    #[inline]
    pub const fn bit_flip(self) -> u64 {
        self.bit_flip
    }

    /// Faults of kind `sync_fail` injected so far.
    #[inline]
    pub const fn sync_fail(self) -> u64 {
        self.sync_fail
    }

    /// Faults of kind `reserve_refused` injected so far.
    #[inline]
    pub const fn reserve_refused(self) -> u64 {
        self.reserve_refused
    }

    /// Faults of kind `punch_refused` injected so far.
    #[inline]
    pub const fn punch_refused(self) -> u64 {
        self.punch_refused
    }

    /// Placed `PlacedFault::Fail` faults injected so far.
    #[inline]
    pub const fn placed_fail(self) -> u64 {
        self.placed_fail
    }

    /// Placed `PlacedFault::FailAfter` faults injected so far.
    #[inline]
    pub const fn placed_fail_after(self) -> u64 {
        self.placed_fail_after
    }
}
