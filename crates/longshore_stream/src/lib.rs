// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One connection's byte stream over `Io`, plain or TLS.
//!
//! Callers ask a [`Stream`] to send bytes from a buffer or receive bytes into
//! one. Plain streams pass the operation straight to `Io`. With the `tls`
//! feature enabled, TLS streams seal records from the source buffer into a
//! send buffer and open received records into the destination buffer; the
//! owner sees plaintext lengths either way.

mod error;
#[cfg(feature = "tls-core")]
mod handshake;
mod idle;
#[cfg(feature = "tls-core")]
mod records;
mod ring;
mod stream;

pub use error::{TlsError, KEY_RECORDS_RETIRE};
pub use idle::IdleCheck;
pub use ring::{RingBytes, RingDst, RingStep, RING_DST_MIN};
pub use stream::{Step, Stream};
