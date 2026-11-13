// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! S3 store on real io_uring against a real bucket, over plain HTTP or,
//! with S3_TLS=1, HTTPS (handshake by rustls, records by longshore::tls).
//!
//! Reads S3_ENDPOINT, S3_REGION, S3_BUCKET, S3_ACCESS_KEY and S3_SECRET_KEY
//! from the environment, and S3_SESSION_TOKEN too when the keys are
//! temporary. With S3_VENDOR=gcs it speaks Google Cloud Storage's
//! XML API and reads S3_BEARER_TOKEN instead of S3_REGION and the two keys.
//! Hostname resolution uses the in-tree resolver.
//!
//!   s3_uring selftest [--size BYTES]
//!       store, fetch (whole and ranged), store_no_clobber and erase one
//!       object, checking every byte, with heap allocation sealed.
//!   s3_uring multipart [--size BYTES]
//!       Upload one object as three parts of BYTES each, in parallel,
//!       complete it, read it back by range and erase it; then begin,
//!       store one part of and abort a second upload.
//!   s3_uring badcrc [--size BYTES]
//!       Store one object with a deliberately wrong checksum and report
//!       whether the store refuses it.
//!   s3_uring list PREFIX
//!       Print the keys under PREFIX and their total size.
//!   s3_uring clean PREFIX
//!       Erase every object under PREFIX (which must not be empty).
//!   s3_uring put|put-crc|get [--conns N] [--size BYTES] [--secs S] [--cpu N]
//!       Throughput of repeated stores or fetches; JSON result.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("s3_uring needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
#[path = "s3_uring/linux.rs"]
mod linux;
