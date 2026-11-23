// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How the check programs stop on a failure: a message on standard error and
//! exit status 1, not a panic. Each program includes this file as `mod die`.

use std::fmt::{Debug, Display};
use std::panic::Location;

/// Print `<program>: <msg>` to standard error and exit with status 1.
pub(crate) fn die(msg: impl Display) -> ! {
    // A failure inside a sealed region may still need to allocate to say why.
    longshore::io::alloc_guard::allow_alloc(|| {
        eprintln!("{}: {msg}", env!("CARGO_BIN_NAME"));
    });
    std::process::exit(1)
}

/// The value inside a `Result` or an `Option`, or the end of the program.
pub(crate) trait OrDie<T> {
    /// The value, or [`die`] naming what failed, why, and where.
    fn or_die(self, what: &str) -> T;
}

impl<T, E: Debug> OrDie<T> for Result<T, E> {
    #[track_caller]
    fn or_die(self, what: &str) -> T {
        match self {
            Ok(v) => v,
            Err(e) => die(format_args!("{what}: {e:?} (at {})", Location::caller())),
        }
    }
}

impl<T> OrDie<T> for Option<T> {
    #[track_caller]
    fn or_die(self, what: &str) -> T {
        match self {
            Some(v) => v,
            None => die(format_args!("{what} (at {})", Location::caller())),
        }
    }
}
