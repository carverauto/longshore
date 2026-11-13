// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP client on real io_uring.
//!
//!   http_uring selftest
//!       Client and in-tree server on one ring over loopback: PUT and GET an
//!       8 MiB object, verify the bytes, with heap allocation sealed.
//!   http_uring https HOST IPV4 [PATH]
//!       One HTTPS GET through the full stack (rustls handshake, osc-tls
//!       records) to a real server, with a 10 s timeout. Prints the outcome.
//!   http_uring sink --port P [--size BYTES] [--tls-cert FILE]
//!       Multi-threaded HTTP peer for throughput runs. PUT bodies are discarded
//!       without copying; GET returns --size bytes.
//!   http_uring put|get --addr H:P[,H:P...] [--conns N] [--size BYTES] [--secs S] [--cpu N] [--tls-cert FILE] [--batch BYTES] [--crc 1]
//!       One io_uring thread issuing requests on N connections. Reports GB/s
//!       (GB = 10^9 bytes) and CPU use as JSON. Several comma-separated
//!       addresses spread the connections over several links: connection i
//!       uses address i mod the count. With TLS, --batch caps the ciphertext
//!       sealed before each send (default 1 MiB). --crc 1 checksums every PUT
//!       body with CRC-64/NVME and sends it as `x-amz-checksum-crc64nvme`
//!       (the sink ignores it; this measures the client's cost).
//!
//! With --tls-cert the sink generates a certificate for the name `bench.test`,
//! writes it to FILE (DER) and serves TLS 1.3; the client trusts the
//! certificate in FILE and speaks TLS through its own record layer.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("http_uring needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: osc_io::alloc_guard::Guard = osc_io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
#[path = "http_uring/linux.rs"]
mod linux;
