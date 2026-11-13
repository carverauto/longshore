// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP/1.1 for the object storage client.
//!
//! `head` is sans-I/O: it writes request heads into a caller-provided buffer
//! and parses response (and, for the test server, request) heads out of one.
//! It performs no I/O and no allocation, and enforces explicit limits.
//!
//! `client` is the connection pool; `server` is a scriptable object server
//! used by tests and benchmarks. Both are generic over `crate::io::Io`.

pub mod chunked;
pub mod client;
pub mod head;
pub mod server;
pub mod stream;
