// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The plan's arithmetic (contract C2). Skeleton: filled by group 1.

use crate::{ConfigError, FileConfig, FileSizes};
use longshore_common::io::Limits;

pub(crate) fn plan(
    _limits: Limits,
    _sizes: FileSizes,
) -> Result<(Limits, FileConfig), ConfigError> {
    Err(ConfigError::Ops)
}
