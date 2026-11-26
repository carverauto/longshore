// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! When the peer of a benchmark run stops.

use crate::die::die;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// When a peer stops: when told, or when as many connections have closed
/// as the run has.
pub(crate) struct Stop {
    told: AtomicBool,
    closed: AtomicU32,
    closes: u32,
    /// When a peer alone gives up: its load never came, came with other
    /// options, or died on the way.
    patience: Option<Instant>,
}

impl Stop {
    /// Stops when told, or after `closes` connections closed; zero is
    /// only when told.
    pub(super) fn after(closes: u32) -> Stop {
        Stop {
            told: AtomicBool::new(false),
            closed: AtomicU32::new(0),
            closes,
            patience: None,
        }
    }

    /// The same, and the run ends as failed if it has not stopped
    /// `within` this long.
    pub(super) fn after_within(closes: u32, within: Duration) -> Stop {
        Stop {
            patience: Some(Instant::now() + within),
            ..Stop::after(closes)
        }
    }

    /// Stop now.
    pub(super) fn tell(&self) {
        self.told.store(true, Ordering::Release);
    }

    pub(crate) fn now(&self) -> bool {
        if self.patience.is_some_and(|end| Instant::now() > end) {
            let closed = self.closed.load(Ordering::Acquire);
            die(format_args!(
                "the load did not finish: {closed} of {} connections came and closed",
                self.closes
            ));
        }
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
