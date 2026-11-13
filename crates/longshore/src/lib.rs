// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! An object storage client for Linux `io_uring`.
//!
//! One thread drives everything from completions. Buffers, connection slots
//! and operation slots are allocated when the client is built, and object
//! bodies go to the network from the caller's registered buffers without
//! being copied.
//!
//! - [`io`]: the [`io::Io`] trait every layer is written against, and the
//!   `io_uring` backend.
//! - [`dns`], [`http`], [`tls`]: the protocols.
//! - [`checksum`], [`crypto`]: checksums and request-signing primitives.
//! - [`store`]: the object store API: operations, signing, retries, listing
//!   and credentials.
//! - [`client`]: a handle any thread can share, over one or more I/O
//!   threads that each run a store.
//! - `sim` (feature `sim`): a deterministic simulator that stands in for the
//!   kernel in tests.

pub mod checksum;
pub mod client;
pub mod crypto;
pub mod dns;
pub mod http;
pub mod io;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub mod store;
pub mod tls;
