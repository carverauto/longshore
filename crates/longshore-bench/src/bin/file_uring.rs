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
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    let mut dir_arg: Option<std::path::PathBuf> = None;
    let mut posix = false;
    let mut do_bench = false;
    let mut chain_first = false;
    let mut interleave = false;
    let mut iters = 400usize;
    let mut memlock_child = false;
    let mut do_throughput = false;
    let (mut size_gib, mut block_kib, mut depth, mut runs) = (16u64, 1024u64, 32u64, 3u64);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                let Some(val) = args.next() else {
                    eprintln!("--dir requires a path argument");
                    std::process::exit(2);
                };
                dir_arg = Some(std::path::PathBuf::from(val));
            }
            "--posix" => posix = true,
            "--bench" => do_bench = true,
            "--chain-first" => chain_first = true,
            "--interleave" => interleave = true,
            "--iters" => {
                let Some(val) = args.next() else {
                    eprintln!("--iters requires a count");
                    std::process::exit(2);
                };
                iters = val.parse().unwrap_or_else(|e| {
                    eprintln!("invalid --iters {val}: {e}");
                    std::process::exit(2);
                });
                if iters == 0 {
                    eprintln!("--iters must be at least 1");
                    std::process::exit(2);
                }
            }
            "--memlock-child" => memlock_child = true,
            "--throughput" => do_throughput = true,
            "--size-gib" => size_gib = number(&mut args, "--size-gib"),
            "--block-kib" => block_kib = number(&mut args, "--block-kib"),
            "--depth" => depth = number(&mut args, "--depth"),
            "--runs" => runs = number(&mut args, "--runs"),
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }

    if memlock_child {
        checks::run_memlock_child();
        return;
    }

    let base = dir_arg.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let work = match WorkDir::create(&base) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("file_uring: {e}");
            std::process::exit(1);
        }
    };

    if do_bench {
        let order = if interleave {
            bench::Order::Interleaved
        } else {
            bench::Order::Blocks { chain_first }
        };
        bench::run_direct_sync_bench(&work.0, iters, order);
        return;
    }

    if do_throughput {
        throughput::run(
            &work.0,
            throughput::Shape {
                size: size_gib << 30,
                block: (block_kib << 10) as u32,
                depth: depth as u32,
                runs: runs as u32,
            },
        );
        return;
    }

    checks::run_all(&work.0, posix);
    println!("ok file_uring (16401 buffers, direct-io, submit-all, rlimit-checks)");
}
