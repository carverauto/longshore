// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS 1.3 for the object storage client.
//!
//! `record` is the record layer: it seals application data read from the
//! caller's buffer directly into a send buffer, and opens received records
//! directly into the caller's buffer. It performs no I/O and no allocation.
//!
//! `handshake` wraps rustls for connection establishment only. It performs
//! no I/O either, but it does allocate, inside an explicit exception to the
//! allocation guard. When it finishes, its keys are installed in `record`.

pub mod handshake;
pub mod record;
