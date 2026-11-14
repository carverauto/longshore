// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Failures a test can cause in a ring that is running, so that what the
//! owner of a broken ring does is exercised on a real kernel. Compiled only
//! with the `fault-injection` feature.

use super::UringIo;

/// A way for a ring to fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The next `tick` returns an error. The ring still works, so
    /// [`UringIo::quiesce`] can stop what is in flight.
    TickFails,
    /// The kernel refuses every later `io_uring_enter`, as it would for a
    /// descriptor that is no longer a ring. Nothing in flight can be
    /// cancelled or reaped, so `quiesce` fails and the memory the kernel
    /// may still use is leaked.
    RingLost,
}

impl UringIo {
    /// Make the ring fail in the given way.
    pub fn inject(&mut self, fault: Fault) {
        match fault {
            Fault::TickFails => self.fail_tick = true,
            Fault::RingLost => self.ring.lose(),
        }
    }
}
