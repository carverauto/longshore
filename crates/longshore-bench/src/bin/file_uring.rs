// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel checks and direct-I/O chain latency benchmark for file operations.
//!
//!   file_uring [--dir PATH] [--posix] [--bench] [--chain-first | --interleave] [--iters N]
//!   file_uring [--dir PATH] --throughput [--size-gib N] [--block-kib N] [--depth N] [--runs N]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("file_uring needs Linux (io_uring)");
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

#[cfg(target_os = "linux")]
#[path = "file_uring/bench.rs"]
mod bench;
#[cfg(target_os = "linux")]
#[path = "file_uring/checks.rs"]
mod checks;

#[cfg(target_os = "linux")]
#[path = "file_uring/throughput.rs"]
mod throughput;

#[cfg(target_os = "linux")]
fn number(args: &mut impl Iterator<Item = String>, flag: &str) -> u64 {
    match args.next().map(|v| v.parse::<u64>()) {
        Some(Ok(n)) if n > 0 => n,
        _ => {
            eprintln!("{flag} requires a count of at least 1");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "linux")]
struct WorkDir(std::path::PathBuf);

#[cfg(target_os = "linux")]
impl WorkDir {
    fn create(base: &std::path::Path) -> Result<Self, String> {
        if !base.is_dir() {
            return Err(format!("directory does not exist: {}", base.display()));
        }
        let dir = base.join(format!(".longshore-file-uring-{}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create work dir {}: {e}", dir.display()))?;
        Ok(Self(dir))
    }
}

#[cfg(target_os = "linux")]
impl Drop for WorkDir {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop must not panic over a leftover temporary file.
        let _: Option<()> = std::fs::remove_dir_all(&self.0).ok();
    }
}

/// What the command line asked for.
#[cfg(target_os = "linux")]
struct Args {
    dir: Option<std::path::PathBuf>,
    posix: bool,
    bench: bool,
    chain_first: bool,
    interleave: bool,
    iters: usize,
    memlock_child: bool,
    throughput: bool,
    size_gib: u64,
    block_kib: u64,
    depth: u64,
    runs: u64,
}

/// The count after `--iters`, or exit status 2.
#[cfg(target_os = "linux")]
fn iters(args: &mut impl Iterator<Item = String>) -> usize {
    let Some(val) = args.next() else {
        eprintln!("--iters requires a count");
        std::process::exit(2);
    };
    let iters = val.parse().unwrap_or_else(|e| {
        eprintln!("invalid --iters {val}: {e}");
        std::process::exit(2);
    });
    if iters == 0 {
        eprintln!("--iters must be at least 1");
        std::process::exit(2);
    }
    iters
}

/// Parse the command line; exit status 2 on a flag it does not know.
#[cfg(target_os = "linux")]
fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut a = Args {
        dir: None,
        posix: false,
        bench: false,
        chain_first: false,
        interleave: false,
        iters: 400,
        memlock_child: false,
        throughput: false,
        size_gib: 16,
        block_kib: 1024,
        depth: 32,
        runs: 3,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                let Some(val) = args.next() else {
                    eprintln!("--dir requires a path argument");
                    std::process::exit(2);
                };
                a.dir = Some(std::path::PathBuf::from(val));
            }
            "--posix" => a.posix = true,
            "--bench" => a.bench = true,
            "--chain-first" => a.chain_first = true,
            "--interleave" => a.interleave = true,
            "--iters" => a.iters = iters(&mut args),
            "--memlock-child" => a.memlock_child = true,
            "--throughput" => a.throughput = true,
            "--size-gib" => a.size_gib = number(&mut args, "--size-gib"),
            "--block-kib" => a.block_kib = number(&mut args, "--block-kib"),
            "--depth" => a.depth = number(&mut args, "--depth"),
            "--runs" => a.runs = number(&mut args, "--runs"),
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

#[cfg(target_os = "linux")]
fn main() {
    let args = parse_args();
    if args.memlock_child {
        checks::run_memlock_child();
        return;
    }

    let base = args
        .dir
        .unwrap_or_else(|| std::env::current_dir().or_die("cwd"));
    let work = match WorkDir::create(&base) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("file_uring: {e}");
            std::process::exit(1);
        }
    };

    if args.bench {
        let order = if args.interleave {
            bench::Order::Interleaved
        } else {
            bench::Order::Blocks {
                chain_first: args.chain_first,
            }
        };
        bench::run_direct_sync_bench(&work.0, args.iters, order);
        return;
    }

    if args.throughput {
        throughput::run(
            &work.0,
            throughput::Shape {
                size: args.size_gib << 30,
                block: (args.block_kib << 10) as u32,
                depth: args.depth as u32,
                runs: args.runs as u32,
            },
        );
        return;
    }

    checks::run_all(&work.0, args.posix);
    println!("ok file_uring (16401 buffers, direct-io, submit-all, rlimit-checks)");
}
