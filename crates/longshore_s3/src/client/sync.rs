// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The locks, atomics and thread parking that callers and I/O threads meet
//! on. They are the standard library's, except in a build with
//! `--cfg longshore_model`, where they are `loom`'s so that a model checker
//! can run the client's hand-offs in every order. See `tests/client/model.rs`.

#[cfg(not(longshore_model))]
pub(super) use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
#[cfg(not(longshore_model))]
pub(super) use std::sync::{Condvar, Mutex};
#[cfg(not(longshore_model))]
pub(super) use std::thread::{current, park, Thread};

#[cfg(longshore_model)]
pub(super) use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
#[cfg(longshore_model)]
pub(super) use loom::sync::{Condvar, Mutex};
#[cfg(longshore_model)]
pub(super) use loom::thread::{current, park, Thread};
