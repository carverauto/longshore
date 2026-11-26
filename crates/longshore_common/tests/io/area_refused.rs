// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A receive area that cannot be had is refused at build, with an error
//! that names zero-copy receive: on the portable backend always, and on the
//! ring for an interface that has none (the loopback), whatever the kernel
//! and the capabilities of the test: no feature, no `CAP_NET_ADMIN`, no
//! driver support or no split of headers each give such an error.

#![cfg(unix)]

use longshore_common::io::Limits;

/// The loopback's index, as `if_nametoindex("lo")` gives it on Linux.
const LOOPBACK: u32 = 1;

fn limits() -> Limits {
    Limits::BASE
        .with_sockets_max(2)
        .with_ops_max(8)
        .with_area_pages(64)
        .with_area_queue(LOOPBACK, 0)
}

#[test]
fn the_portable_backend_refuses_an_area() {
    let Err(e) = longshore_common::io::posix::PosixIo::new(limits()) else {
        panic!("PosixIo::new took an area of pages");
    };
    assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
    assert!(e.to_string().contains("zero-copy receive"), "{e}");
}

#[test]
fn the_portable_backend_without_an_area_builds() {
    let built = longshore_common::io::posix::PosixIo::new(limits().with_area_pages(0));
    assert!(built.is_ok(), "{:?}", built.err());
}

#[cfg(target_os = "linux")]
#[test]
fn the_ring_refuses_an_area_on_the_loopback_naming_why() {
    let Err(e) = longshore_common::io::uring::UringIo::new(limits()) else {
        panic!("UringIo::new registered an area on the loopback");
    };
    let said = e.to_string();
    eprintln!("refused: {said}");
    assert!(said.contains("zero-copy receive"), "{said}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_ring_without_an_area_builds_with_the_same_limits() {
    let built = longshore_common::io::uring::UringIo::new(limits().with_area_pages(0));
    assert!(built.is_ok(), "{:?}", built.err());
}
