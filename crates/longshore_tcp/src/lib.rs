// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TCP connections over `Io`: a fixed set of connections and listeners,
//! framing, frames lent in place, and time owned by the caller.
//!
//! [`Net`] is the set; [`KernelTransport`] carries it over the kernel's TCP
//! through the `Io` trait (io_uring, the portable backend, the simulator).
//! See [`net`] for how one turn of the caller's loop goes.

// TLS connections run through these (task 3.3 of `add-tcp-crate`).
use longshore_stream as _;
#[cfg(feature = "tls-core")]
use longshore_tls as _;

pub mod addr;
pub mod backoff;
pub mod check;
pub mod config;
pub mod deadline;
pub mod event;
pub mod framing;
pub mod handle;
pub mod kernel;
pub mod net;
mod sizes;
pub mod transport;

pub use addr::{Addr, PathError, ServerName, UnixPath};
pub use backoff::Backoff;
pub use config::{
    Config, ConfigError, ConnOpts, ListenConfig, Reconnect, Rx, Sizes, TLS_SEGMENT_MIN,
};
pub use deadline::Deadlines;
pub use event::{Event, NetError, SendError, Why};
pub use framing::{FrameError, Framing, FramingError, HeaderFraming};
pub use handle::{Conn, Generations, Listener};
pub use kernel::KernelTransport;
#[cfg(feature = "tls-core")]
pub use kernel::Tls;
pub use net::{Frame, Net};
pub use transport::{Happened, Listening, Opened, Refused, Role, Span, Transport, TLS_FAILED};
