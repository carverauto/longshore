// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests of the public API, under an allocator that aborts if the
//! data path allocates.

#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(test)]
#[path = "e2e/mod.rs"]
mod e2e;
