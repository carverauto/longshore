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
//! This crate holds no code. It gathers the crates the client is made of
//! under one name:
//!
//! - [`io`], from `longshore_common`: the [`io::Io`] trait every layer is
//!   written against, and the `io_uring` backend.
//! - [`dns`], [`http`], [`tls`]: the protocols, from `longshore_dns`,
//!   `longshore_http_client` and `longshore_tls`.
//! - [`checksum`], from `longshore_crc`.
//! - [`store`], from `longshore_s3`: the object store API: operations,
//!   signing, retries, listing and credentials.
//! - [`client`], from `longshore_s3`: a handle any thread can share, over
//!   one or more I/O threads that each run a store.
//! - [`crypto`], from `longshore_s3`: the request-signing primitives.
//! - `sim` (feature `sim`), from `longshore_common`: a deterministic
//!   simulator that stands in for the kernel in tests.

pub use longshore_common::io;
#[cfg(feature = "sim")]
pub use longshore_common::sim;
pub use longshore_crc as checksum;
pub use longshore_dns as dns;
pub use longshore_http_client as http;
pub use longshore_s3 as store;
pub use longshore_s3::{client, crypto};
pub use longshore_tls as tls;
