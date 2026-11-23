// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Failing forms of the calls the contract checks make.
//!
//! The contract checks (`contract`, `contract_chains`, `contract_files`,
//! `contract_net`) are tests that ship in the library, so that every backend
//! and every crate's test suite runs the same ones. A call a check needs to
//! succeed fails the check when it does not: these forms say which call it
//! was, and [`fail`] is the one place a check stops.

use core::fmt;

/// Stop the running contract check with `why`.
#[track_caller]
#[expect(
    clippy::panic,
    reason = "a contract check is a test: stopping it with a message is how it fails"
)]
pub(crate) fn fail(why: fmt::Arguments<'_>) -> ! {
    panic!("{why}")
}
