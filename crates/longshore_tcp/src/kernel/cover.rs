// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Which cells of the TLS phase table a kernel transport has been in:
//! how often each event reached a connection slot in each phase. Counted
//! in debug builds only, for the model test's report of what it reached.

use super::KernelTransport;
use crate::hits::Hits;
use longshore_common::io::Io;

/// How often each event reached a slot in each phase of its TLS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PhaseCoverage {
    hits: Hits<{ PhaseCoverage::PHASES.len() }, { PhaseCoverage::EVENTS.len() }>,
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
    pub const NONE: PhaseCoverage = PhaseCoverage { hits: Hits::NONE };

    /// How often event `event` reached a slot in phase `phase`, by their
    /// places in [`PhaseCoverage::PHASES`] and [`PhaseCoverage::EVENTS`].
    pub const fn hits(&self, phase: usize, event: usize) -> u32 {
        self.hits.get(phase, event)
    }

    /// Count event `event` in phase `phase`.
    #[cfg(feature = "tls-core")]
    #[inline]
    pub(super) fn count(&mut self, phase: usize, event: usize) {
        self.hits.count(phase, event);
    }

    /// Add what `other` counted.
    pub fn add(&mut self, other: &PhaseCoverage) {
        self.hits.add(&other.hits);
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
