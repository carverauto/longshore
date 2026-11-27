// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy receive on `UringIo`, against a real card: one connection
//! from `tcp_uring bulk`, received into the pages of an area registered for
//! one receive queue of one interface.
//!
//! ```text
//! zc_uring check  <interface> <queue> <address:port> [options]
//! zc_uring log    <interface> <queue> <address:port> --file PATH [options]
//! zc_uring refuse <interface> <queue> <address:port> [options]
//! ```
//!
//!   --frame N    bytes a frame, as the sender's --frame (65536)
//!   --pages N    pages of the area (16384: 64 MiB)
//!   --file PATH  log: where the pages go, opened for direct I/O
//!   --writes N   log: gathered writes in flight (4)
//!   --spans N    log: pages a write at most (256)
//!
//! `check` accepts one connection and receives each frame as a credit of
//! 256 bytes and then one of the rest, checks that a frame's first four
//! bytes are its length less four, big-endian, returns every piece, and
//! prints one JSON line when the peer has finished. `log` does the same and
//! writes every whole page to the file with gathered writes, several in
//! flight, returning a page when its write completes; it adds bytes a
//! second and the thread's CPU-seconds a gigabyte. `refuse` builds the
//! backend and prints why it was refused; it fails if nothing was.

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;

#[cfg(target_os = "linux")]
fn main() {
    run::main();
}

#[cfg(target_os = "linux")]
#[path = "zc_uring/opts.rs"]
mod opts;
#[cfg(target_os = "linux")]
#[path = "zc_uring/run.rs"]
mod run;
