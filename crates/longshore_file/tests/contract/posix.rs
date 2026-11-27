// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The contract suite on the portable backend (`kqueue` on macOS, `epoll`
//! on Linux), in a directory of its own under the system's temporary
//! directory.

use longshore_common::io::posix::PosixIo;
use longshore_file::check;
use std::path::PathBuf;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "longshore_file_{tag}_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop must not panic over a leftover file.
        let _: Option<()> = std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn the_contract_holds_on_the_portable_backend() {
    let dir = TempDir::new("contract");
    let root = format!("{}/", dir.0.display());
    let ran = check::run_all(&mut |l| PosixIo::new(l).unwrap(), &root);
    // macOS opens any file with F_NOCACHE; a Linux temporary directory
    // on tmpfs or overlayfs may refuse O_DIRECT, which run_all reported.
    assert!(ran.direct() || cfg!(target_os = "linux"), "{ran:?}");
}
