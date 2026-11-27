// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel self-check of `longshore::file`: the contract suite
//! (`longshore::file::check::run_all`) on `UringIo`, and with `--posix` on
//! the portable backend too, in a work directory of its own.
//!
//!   files_uring [--dir PATH] [--posix] [--no-direct]
//!
//! A directory that cannot be opened for direct I/O fails the check,
//! unless `--no-direct` says the file system has none (tmpfs).

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("files_uring needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;
#[cfg(target_os = "linux")]
use die::OrDie;

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

/// A directory of the run's own, removed at the end.
#[cfg(target_os = "linux")]
struct WorkDir(std::path::PathBuf);

#[cfg(target_os = "linux")]
impl WorkDir {
    fn create(base: &std::path::Path, tag: &str) -> WorkDir {
        if !base.is_dir() {
            die::die(format_args!("directory does not exist: {}", base.display()));
        }
        let dir = base.join(format!(
            ".longshore-files-uring-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).or_die("create the work directory");
        WorkDir(dir)
    }

    /// The directory as the checks take it: ending in `/`.
    fn root(&self) -> String {
        format!("{}/", self.0.display())
    }
}

#[cfg(target_os = "linux")]
impl Drop for WorkDir {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop must not panic over a leftover file.
        let _: Option<()> = std::fs::remove_dir_all(&self.0).ok();
    }
}

/// `--dir PATH`, `--posix` and `--no-direct`, or exit status 2.
#[cfg(target_os = "linux")]
fn parse_args() -> (Option<std::path::PathBuf>, bool, bool) {
    let mut args = std::env::args().skip(1);
    let (mut dir, mut posix, mut no_direct) = (None, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                let Some(val) = args.next() else {
                    eprintln!("--dir requires a path argument");
                    std::process::exit(2);
                };
                dir = Some(std::path::PathBuf::from(val));
            }
            "--posix" => posix = true,
            "--no-direct" => no_direct = true,
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }
    (dir, posix, no_direct)
}

#[cfg(target_os = "linux")]
fn main() {
    use longshore::file::check;
    use longshore::io::posix::PosixIo;
    use longshore::io::uring::UringIo;

    let (dir, posix, no_direct) = parse_args();
    let base = dir.unwrap_or_else(|| std::env::current_dir().or_die("cwd"));
    let ring = WorkDir::create(&base, "ring");
    let mut ran = vec![check::run_all(
        &mut |l| UringIo::new(l).or_die("UringIo::new"),
        &ring.root(),
    )];
    if posix {
        let portable = WorkDir::create(&base, "posix");
        ran.push(check::run_all(
            &mut |l| PosixIo::new(l).or_die("PosixIo::new"),
            &portable.root(),
        ));
    }
    let direct = ran.iter().all(check::Ran::direct);
    if !direct && !no_direct {
        die::die(format_args!(
            "no direct I/O in {}: the direct-I/O checks did not run \
             (--no-direct if the file system has none)",
            base.display()
        ));
    }
    let on = if posix {
        "io_uring and epoll"
    } else {
        "io_uring"
    };
    let left_out = if direct { "" } else { ", direct I/O left out" };
    println!("ok files_uring (the file contract on {on}{left_out})");
}
