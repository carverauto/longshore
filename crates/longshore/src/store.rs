// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Object storage API over the HTTP client: request signing, vendor request
//! builders, retry classification.

pub mod creds;
pub mod list;
pub mod multipart;
pub mod presign;
pub mod retry;
pub mod s3;
pub mod sharedkey;
pub mod sigv4;
pub mod time;
