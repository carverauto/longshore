// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How the check programs stop on a failure: a message on standard error and
//! exit status 1, not a panic. Each program includes this file as `mod die`.

use std::fmt::Display;

/// Print `<program>: <msg>` to standard error and exit with status 1.
pub(crate) fn die(msg: impl Display) -> ! {
    // A failure inside a sealed region may still need to allocate to say why.
    longshore::io::alloc_guard::allow_alloc(|| {
        eprintln!("{}: {msg}", env!("CARGO_BIN_NAME"));
    });
    std::process::exit(1)
}
