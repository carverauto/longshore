// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The model test of `Net`: seeded random calls on a client set and a
//! server set that share one simulated backend, with the smallest pools,
//! network faults and a seeded order of reports, checked after every step
//! against what a caller may rely on. One run in three protects its
//! connections with TLS, where that is compiled in.
//!
//! The steps of a run are a list made from the seed before it starts, so
//! a failing run can be replayed and cut down: `tcp_model --seed N
//! --steps M --shrink` (the example of that name).

mod book;
pub(crate) mod drive;
mod events;
mod life;
pub(crate) mod steps;
mod world;
