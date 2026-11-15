// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The object store: operations against Amazon S3, S3-compatible stores and
//! Google Cloud Storage, over the HTTP client.
//!
//! There are two ways in.
//!
//! [`client`] is the one most programs want. A [`client::Client`] is a
//! handle any thread can share, over one or more I/O threads. Its calls are
//! futures that can also block, and bodies live in buffers it lends.
//!
//! [`s3`] is the store itself, single-threaded and written against the
//! `Io` trait of `longshore_common`. The caller owns the event loop,
//! submits each operation with a token and collects outcomes by polling.
//! It runs the same on the kernel and in the simulator, which is how it is
//! tested.
//!
//! Around them:
//!
//! - [`creds`]: long-lived keys, temporary keys with a session token, or a
//!   bearer token, and reading them from a file.
//! - [`multipart`], [`list`], [`erase`], [`stat`], [`options`],
//!   [`presign`]: the types of the operations beyond store and fetch.
//! - [`report`]: what `put`, `head` and `get` return, for a caller that
//!   must know exactly what the store answered.
//! - [`retry`]: which failures are tried again, and how often.
//! - [`sigv4`], [`sharedkey`], [`crypto`]: request signing for S3 and for
//!   Azure, and the SHA-256 and HMAC under them.

pub mod client;
pub mod creds;
pub mod crypto;
pub mod erase;
pub mod list;
pub mod multipart;
pub mod options;
pub mod presign;
pub mod report;
pub mod retry;
pub mod s3;
pub mod sharedkey;
pub mod sigv4;
pub mod stat;
pub mod time;
