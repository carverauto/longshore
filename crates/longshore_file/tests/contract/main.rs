// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The contract suite (`longshore_file::check::run_all`) on the simulator
//! and on the portable backend, and the checks only the simulator can make
//! because it can be made to fail: poisoning, short and partial transfers,
//! a write of nothing, errors mapped to one type, a broken chain resumed,
//! a failed close, power loss, and pages of the receive area as spans.

#[cfg(test)]
mod sim;

#[cfg(test)]
mod faults;

#[cfg(test)]
mod power;

#[cfg(test)]
mod resumes;

#[cfg(test)]
mod ring;

#[cfg(test)]
#[cfg(unix)]
mod posix;
