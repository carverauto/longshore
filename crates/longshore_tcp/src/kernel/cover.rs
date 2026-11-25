// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Which cells of the TLS phase table a kernel transport has been in:
//! how often each event reached a connection slot in each phase. Counted
//! in debug builds only, for the model test's report of what it reached.

use super::KernelTransport;
use longshore_common::io::Io;

/// How often each event reached a slot in each phase of its TLS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PhaseCoverage {
    hits: [[u32; PhaseCoverage::EVENTS.len()]; PhaseCoverage::PHASES.len()],
}

impl PhaseCoverage {
    /// The phases, in the order of the table's rows.
    pub const PHASES: [&'static str; 6] = [
        "Plain",
        "Handshake",
        "Aborted",
        "Secure",
        "Alerting",
        "AlertAborted",
    ];

    /// The events, in the order of the table's columns.
    pub const EVENTS: [&'static str; 5] = [
        "secure",
        "a send or receive completion",
        "shutdown_write",
        "close submitted",
        "Closed",
    ];

    /// Nothing counted.
    pub const NONE: PhaseCoverage = PhaseCoverage {
        hits: [[0; PhaseCoverage::EVENTS.len()]; PhaseCoverage::PHASES.len()],
    };

    /// How often event `event` reached a slot in phase `phase`, by their
    /// places in [`PhaseCoverage::PHASES`] and [`PhaseCoverage::EVENTS`].
    pub const fn hits(&self, phase: usize, event: usize) -> u32 {
        self.hits[phase][event]
    }

    /// Count event `event` in phase `phase`.
    #[cfg(feature = "tls-core")]
    #[inline]
    pub(super) fn count(&mut self, phase: usize, event: usize) {
        if cfg!(debug_assertions) {
            let cell = &mut self.hits[phase][event];
            *cell = cell.saturating_add(1);
        }
    }

    /// Add what `other` counted.
    pub fn add(&mut self, other: &PhaseCoverage) {
        for (row, more) in self.hits.iter_mut().zip(other.hits.iter()) {
            for (cell, n) in row.iter_mut().zip(more.iter()) {
                *cell = cell.saturating_add(*n);
            }
        }
    }
}

impl Default for PhaseCoverage {
    fn default() -> PhaseCoverage {
        PhaseCoverage::NONE
    }
}

impl<IO: Io> KernelTransport<IO> {
    /// What has been counted so far; nothing in a release build, and
    /// nothing where no TLS is compiled in.
    pub const fn phase_coverage(&self) -> PhaseCoverage {
        self.secure.coverage()
    }
}
