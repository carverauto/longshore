// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Focused checks of the set on the simulator: the plan, each operation,
//! the bounds, and the faults that end or resume an operation.

mod bounds;
mod faults;
mod ops;
mod plan;
#[cfg(test)]
mod poisoned;
#[cfg(test)]
mod record;
mod recorded;
#[cfg(test)]
mod review;
#[cfg(test)]
mod rig;
