// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Writes and reads one file through `longshore::file`, and for comparison
//! through `longshore_common`'s raw `Op::File*` operations, on one thread.
//! Prints one JSON line a pass: GB/s, operations a second, latency
//! percentiles of the operations and of the syncs, and the thread's CPU.
//!
//!   file_bench [--backend uring|posix] [--api file|raw|both]
//!              [--phase write|read|both] [--direct] [--dsync] [--random]
//!              [--block N] [--write-size N] [--depth N] [--sync-every N]
//!              [--file-size N] [--bytes N | --secs S] [--runs N] [--path P]
//!
//! Sizes take `k`, `m` or `g`. The file is made, reserved and written once,
//! unmeasured, then each run opens it through each API asked for (their
//! order swapped every run), makes a write pass and a read pass, and
//! closes it. The file is removed at the end. See `docs/file.md`.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("file_bench needs Linux or macOS");
    std::process::exit(2);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../die.rs"]
mod die;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "file_bench/opts.rs"]
mod opts;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "file_bench/pass.rs"]
mod pass;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "file_bench/raw.rs"]
mod raw;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "file_bench/set.rs"]
mod set;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use {
    die::OrDie,
    longshore::file::{FileConfig, FileSizes},
    longshore::io::{extents::Extents, BufId, Completions, Io, Limits},
    opts::{Api, Backend, Opts},
};

/// The first token of the set; the raw path's are below it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SET_TOKENS: u64 = 1 << 32;

/// The seed of the offsets of a pass: the same for both APIs of a run.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn seed(run: u32, write: bool) -> u64 {
    0x9E37_79B9_7F4A_7C15 ^ (u64::from(run) << 1 | u64::from(write))
}

/// The backend's `Limits`, with the set planned on top when it runs: the
/// raw path has file slot 0, small buffer 0 and `depth + 2` operations.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn plan(o: &Opts) -> (Limits, Option<FileConfig>) {
    let base = Limits::BASE
        .with_sockets_max(2)
        .with_small_buffers(1)
        .with_small_buffer_size(4096)
        .with_files_max(1)
        .with_aligned_buffers(o.depth)
        .with_aligned_buffer_size(o.write)
        .with_block_size(o.block)
        .with_ops_max(o.depth + 2);
    if !o.with_set() {
        return (base, None);
    }
    // A sync beside `depth` transfers: the file's own slot and `depth`
    // shared ones.
    let sizes = FileSizes::new(1, o.depth + 1)
        .with_shared(o.depth)
        .with_paths(1)
        .with_token_base(SET_TOKENS);
    let (limits, config) = FileConfig::plan(base, sizes).or_die("FileConfig::plan");
    (limits, Some(config))
}

/// Every run on `io`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run<IO: Io>(o: &Opts, mut io: IO, limits: &Limits, config: Option<FileConfig>) {
    let mut out = Completions::new(limits.ops_max() as usize);
    let mut x = Extents::new(limits.aligned(0), o.depth);
    let exts: Vec<_> = (0..o.depth).map(|_| x.take().or_die("an extent")).collect();
    let bufs: Vec<BufId> = exts.iter().map(|e| e.buf()).collect();
    fill(&mut io, &bufs);
    let mut raw = raw::Raw::new(&mut io, o, limits, &bufs);
    raw.prepare(&mut io, &mut out, o);
    let mut set = config.map(|c| set::Set::new(c, limits, &exts));
    for n in 1..=o.runs {
        for &api in o.apis(n) {
            match (api, set.as_mut()) {
                (Api::Raw, _) => raw.run(&mut io, &mut out, o, n),
                (Api::File, Some(s)) => s.run(&mut io, (&mut out, &mut x), o, n),
                (Api::File, None) => die::die("the set was not planned"),
            }
        }
    }
    drop(io);
    // Best effort: a file left behind does not change the results.
    let _: Option<()> = std::fs::remove_file(&o.path).ok();
}

/// Incompressible contents, different in every buffer.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn fill<IO: Io>(io: &mut IO, bufs: &[BufId]) {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for &b in bufs {
        for chunk in io.buf(b).as_chunks_mut::<8>().0 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *chunk = x.to_le_bytes();
        }
    }
}

#[cfg(target_os = "linux")]
fn uring(o: &Opts, limits: &Limits, config: Option<FileConfig>) {
    let io = longshore::io::uring::UringIo::new(*limits).or_die("UringIo::new");
    run(o, io, limits, config);
}

#[cfg(target_os = "macos")]
fn uring(_: &Opts, _: &Limits, _: Option<FileConfig>) {
    die::die("--backend uring needs Linux");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() {
    let o = Opts::parse(std::env::args().skip(1));
    let (limits, config) = plan(&o);
    match o.backend {
        Backend::Uring => uring(&o, &limits, config),
        Backend::Posix => {
            let io = longshore::io::posix::PosixIo::new(limits).or_die("PosixIo::new");
            run(&o, io, &limits, config);
        }
    }
}
