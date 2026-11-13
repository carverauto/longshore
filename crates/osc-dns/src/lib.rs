// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Non-blocking DNS for a fixed set of hostnames.
//!
//! `codec` is sans-I/O: it encodes queries and parses responses and never
//! reads outside the bytes it is given. `resolver` drives it over `osc_io::Io`,
//! resolves every configured hostname at startup and refreshes on a fixed
//! interval, ignoring record TTLs. Lookups on the request path read an
//! in-memory cache and perform no I/O.

pub mod codec;
pub mod resolver;

pub use resolver::{Config, Resolver};
