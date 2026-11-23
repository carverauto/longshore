// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 8: `IORING_SETUP_SUBMIT_ALL` & consumed check (`#42`).

use super::f08_uring_submit_all::{file_limits, open_flags};
#[cfg(unix)]
use crate::e2e::harness::posix_backend;
use crate::e2e::harness::{drive_until, write_nul_path, write_pool_bytes, TempDir};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(target_os = "linux")]
use longshore::io::Busy;
#[cfg(target_os = "linux")]
use longshore::io::ENOENT;
use longshore::io::{BufId, Completions, FileSlot, Io, Op, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
#[test]
fn t2_f08_1_uring_refused_ops_at_head_mid_tail_of_ops_max_batch() {
    let dir = TempDir::new("f08_pos").unwrap();
    let mut io = UringIo::new(file_limits(8)).expect("uring");
    let mut out = Completions::new(16);
    let missing = dir.file_path("none.dat");
    let mlen = write_nul_path(&mut io, BufId(0), 0, missing.to_str().unwrap());

    for idx in 0..8u64 {
        let op = if matches!(idx, 0 | 3 | 6) {
            Op::FileOpen {
                slot: FileSlot(idx as u32),
                path_buf: BufId(0),
                path_off: 0,
                path_len: mlen,
                flags: open_flags(false),
            }
        } else {
            Op::Timeout { nanos: 0 }
        };
        io.submit(Token(1 + idx), op).unwrap();
    }
    let mut cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 8).unwrap();
    cs.sort_by_key(|c| c.token.0);
    for (idx, c) in cs.iter().enumerate() {
        if matches!(idx, 0 | 3 | 6) {
            assert_eq!(c.result, -ENOENT, "index {idx}");
        } else {
            assert_eq!(c.result, 0, "index {idx}");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn t2_f08_2_uring_full_ops_max_batch_consumed_and_next_busy() {
    let mut io = UringIo::new(file_limits(8)).expect("uring");
    let mut out = Completions::new(16);
    for i in 0..8u64 {
        io.submit(Token(1 + i), Op::Timeout { nanos: 1_000_000 })
            .unwrap();
    }
    assert_eq!(io.submit(Token(99), Op::Timeout { nanos: 0 }), Err(Busy));
    let cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 8).unwrap();
    assert!(cs.iter().all(|c| c.result == 0));
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn t2_f08_3_uring_failing_op_followed_by_valid_chain() {
    let dir = TempDir::new("f08_chain").unwrap();
    let mut io = UringIo::new(file_limits(16)).expect("uring");
    let mut out = Completions::new(16);

    let good = dir.file_path("good.dat");
    let glen = write_nul_path(&mut io, BufId(0), 0, good.to_str().unwrap());
    io.submit(
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: glen,
            flags: open_flags(true),
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 1_000, |v| !v.is_empty()).unwrap();

    let missing = dir.file_path("missing.dat");
    let mlen = write_nul_path(&mut io, BufId(1), 0, missing.to_str().unwrap());
    write_pool_bytes(&mut io, BufId(2), 0, b"chained-payload");

    io.submit(
        Token(10),
        Op::FileOpen {
            slot: FileSlot(1),
            path_buf: BufId(1),
            path_off: 0,
            path_len: mlen,
            flags: open_flags(false),
        },
    )
    .unwrap();
    io.submit_chain(&[
        (
            Token(11),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 15,
                file_off: 0,
            },
        ),
        (
            Token(12),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();

    let mut cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 3).unwrap();
    cs.sort_by_key(|c| c.token.0);
    assert_eq!(cs[0].result, -ENOENT);
    assert_eq!(cs[1].result, 15);
    assert_eq!(cs[2].result, 0);
}

#[test]
fn t2_f08_4_file_uring_binary_and_scripts_include_submit_all_check() {
    let root = crate::e2e::harness::workspace_root();
    let script = std::fs::read_to_string(root.join("scripts/test-linux.sh")).expect("script");
    assert!(
        script.contains("file_uring"),
        "scripts/test-linux.sh must wire file_uring"
    );
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("ci.yml");
    assert!(ci.contains("file_uring"), "ci.yml must wire file_uring");

    let file_uring_rs = root.join("crates/longshore-bench/src/bin/file_uring.rs");
    let file_uring_main = root.join("crates/longshore-bench/src/bin/file_uring/main.rs");
    assert!(
        file_uring_rs.exists() || file_uring_main.exists(),
        "file_uring check binary source must exist"
    );

    #[cfg(target_os = "linux")]
    {
        let bin = std::env::var_os("LONGSHORE_FILE_URING_BIN")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| {
                std::env::current_exe().ok().and_then(|exe| {
                    [
                        exe.parent().map(|d| d.join("file_uring")),
                        exe.parent()
                            .and_then(|d| d.parent())
                            .map(|d| d.join("file_uring")),
                    ]
                    .into_iter()
                    .flatten()
                    .find(|p| p.is_file())
                })
            });
        if let Some(bin) = bin {
            let dir = TempDir::new_in_test_dir("f08_submit_all").expect("temp dir");
            let out = std::process::Command::new(bin)
                .args(["--dir", &dir.path().to_string_lossy()])
                .output()
                .expect("run file_uring");
            assert!(
                out.status.success(),
                "file_uring failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[test]
fn t2_f08_5_engine_backends_match_uring_invalid_head_never_strands_tail() {
    let run = |io: &mut dyn Io, dir: &Path| {
        let mut out = Completions::new(16);
        let good = dir.join("tail.dat");
        let glen = write_nul_path(io, BufId(0), 0, good.to_str().unwrap());
        write_pool_bytes(io, BufId(1), 0, b"head-bad");
        // Unopened FileSlot(3) FileWrite at head, followed by valid Timeout and FileOpen.
        io.submit(
            Token(1),
            Op::FileWrite {
                slot: FileSlot(3),
                buf: BufId(1),
                off: 0,
                len: 8,
                file_off: 0,
            },
        )
        .unwrap();
        io.submit(Token(2), Op::Timeout { nanos: 0 }).unwrap();
        io.submit(
            Token(3),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: glen,
                flags: open_flags(true),
            },
        )
        .unwrap();
        let mut cs = drive_until(io, &mut out, 1_000, |v| v.len() == 3).unwrap();
        cs.sort_by_key(|c| c.token.0);
        assert!(cs[0].result < 0, "unopened FileSlot write must fail");
        assert_eq!(cs[1].result, 0);
        assert_eq!(cs[2].result, 0);
        io.submit(Token(4), Op::FileClose { slot: FileSlot(0) })
            .unwrap();
        let _: Vec<longshore::io::Completion> =
            drive_until(io, &mut out, 1_000, |v| !v.is_empty()).unwrap();
    };
    #[cfg(feature = "sim")]
    {
        let mut io = SimIo::new(file_limits(16), 9, Faults::default());
        io.setup_disk(longshore::sim::DiskConfig {
            max_files: 8,
            max_dirs: 4,
            max_bytes_per_file: 64 * 1024,
            max_name_len: 128,
            write_log_entries: 16,
            write_log_bytes: 32 * 1024,
            read_error_sectors: 4,
            sector_size: 512,
        });
        run(&mut io, Path::new("/"));
    }
    #[cfg(unix)]
    {
        let dir = TempDir::new("f08_tail").unwrap();
        let mut io = posix_backend(file_limits(16)).expect("posix");
        run(&mut io, dir.path());
    }
}
