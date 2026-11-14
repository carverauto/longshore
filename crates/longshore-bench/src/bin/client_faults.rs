// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The shareable client when its I/O threads fail or are torn down under
//! load, on real io_uring threads against the in-tree server over loopback.
//! Built only with the `fault-injection` feature.
//!
//!   client_faults break tick|ring [--threads N]
//!   client_faults churn [--rounds N]
//!
//! `break` runs callers against the client, makes the first I/O thread's
//! ring fail while they work, checks that the other threads carry on, then
//! fails the rest and checks that every caller still gets an answer.
//! `churn` builds and drops a client over and over with work in flight.
//! Both then check that the I/O threads and their descriptors are gone.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("client_faults needs Linux");
}

#[cfg(target_os = "linux")]
#[path = "client_faults/linux.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}
