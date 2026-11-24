// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TCP connections over `Io`: a fixed set of connections and listeners,
//! framing, frames lent in place, and time owned by the caller.
//!
//! [`Net`] is the set; [`KernelTransport`] carries it over the kernel's TCP
//! and Unix-domain sockets through the `Io` trait (io_uring, the portable
//! backend, the simulator). The caller's loop collects completions, routes
//! them to the set, pulls events and frames, and calls [`Net::poll`]; the
//! set itself never waits. Nothing allocates after it is built, except the
//! TLS handshake.
//!
//! An echo server for frames with a four-byte length prefix. Each frame is
//! sent back from the memory it was received into:
//!
//! ```
//! use longshore_common::io::{Completions, Io, Limits};
//! use longshore_tcp::{
//!     Config, ConnOpts, Event, Framing, HeaderFraming, KernelTransport, ListenConfig, Net, Sizes,
//! };
//! use std::net::SocketAddr;
//!
//! /// `make` builds the backend from the limits planned for the set.
//! fn echo<IO: Io>(make: impl FnOnce(Limits) -> IO, addr: SocketAddr) -> Option<()> {
//!     // No connections of our own, 64 accepted, one listener.
//!     let (limits, config) = Config::plan(Limits::BASE, Sizes::new(0, 64, 1)).ok()?;
//!     let mut io = make(limits);
//!     let mut net = Net::new(KernelTransport::new(&config), config, &limits);
//!     let framing = Framing::Header(HeaderFraming::length_prefix(1 << 20).ok()?);
//!     let how = ListenConfig::new(ConnOpts::new(framing));
//!     let _listener = net.listen(&mut io, addr, how).ok()?;
//!     let mut done = Completions::new(256);
//!     loop {
//!         io.tick(true, &mut done).ok()?;
//!         while let Some(completion) = done.pop() {
//!             let _ours = net.on_completion(&mut io, completion);
//!         }
//!         while let Some(event) = net.next_event() {
//!             let Event::Readable(conn) = event else { continue };
//!             while let Some(frame) = net.next_frame(&mut io, conn).ok()? {
//!                 // Into frame memory if it is not there, then sent from it.
//!                 let held = net.hold(&mut io, frame).ok()?;
//!                 let _queued = net.send_lent(conn, &held);
//!                 net.release(held);
//!             }
//!         }
//!         let now = io.now_ns();
//!         net.poll(&mut io, now);
//!     }
//! }
//! ```
//!
//! A loop with timeouts also sleeps no longer than [`Net::next_deadline`]
//! allows. `docs/tcp.md` in the repository describes the receive modes,
//! frame memory, TLS and sizing.

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
