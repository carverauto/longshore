// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! When the peer of a benchmark run stops.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// When a peer stops: when told, or when as many connections have closed
/// as the run has.
pub(crate) struct Stop {
    told: AtomicBool,
    closed: AtomicU32,
    closes: u32,
}

impl Stop {
    /// Stops when told, or after `closes` connections closed; zero is
    /// only when told.
    pub(super) fn after(closes: u32) -> Stop {
        Stop {
            told: AtomicBool::new(false),
            closed: AtomicU32::new(0),
            closes,
        }
    }

    /// Stop now.
    pub(super) fn tell(&self) {
        self.told.store(true, Ordering::Release);
    }

    pub(crate) fn now(&self) -> bool {
        self.told.load(Ordering::Acquire)
    }

    /// A connection of the peer closed.
    pub(crate) fn closed(&self) {
        let closed = self.closed.fetch_add(1, Ordering::AcqRel) + 1;
        if self.closes != 0 && closed >= self.closes {
            self.told.store(true, Ordering::Release);
        }
    }
}
