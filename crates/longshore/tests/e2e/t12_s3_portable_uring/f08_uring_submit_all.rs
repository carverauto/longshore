// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 8: `IORING_SETUP_SUBMIT_ALL` & consumed check (`#42`).

#[cfg(unix)]
use crate::e2e::harness::posix_backend;
#[cfg(target_os = "linux")]
use crate::e2e::harness::write_pool_bytes;
#[cfg(any(unix, target_os = "linux"))]
use crate::e2e::harness::TempDir;
use crate::e2e::harness::{drive_until, write_nul_path};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(target_os = "linux")]
use longshore::io::Sock;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ENOENT};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::path::Path;

pub(super) fn file_limits(ops_max: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(4)
        .with_files_max(8)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_ops_max(ops_max)
}

pub(super) fn open_flags(create: bool) -> OpenFlags {
    OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(create)
}

fn submit_failing_open_then_trailing<IO: Io + ?Sized>(io: &mut IO, dir: &Path) {
    let mut out = Completions::new(16);
    let missing = dir.join("does_not_exist.dat");
    let good = dir.join("trailing_ok.dat");
    let len_missing = write_nul_path(io, BufId::new(0), 0, missing.to_str().unwrap());
    let len_good = write_nul_path(io, BufId::new(1), 0, good.to_str().unwrap());

    io.submit(
        Token::new(1),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: len_missing,
            flags: open_flags(false),
        },
    )
    .unwrap();
    io.submit(Token::new(2), Op::Timeout { nanos: 0 }).unwrap();
    io.submit(Token::new(3), Op::Timeout { nanos: 0 }).unwrap();
    io.submit(
        Token::new(4),
        Op::FileOpen {
            slot: FileSlot::new(1),
            path_buf: BufId::new(1),
            path_off: 0,
            path_len: len_good,
            flags: open_flags(true),
        },
    )
    .unwrap();

    let mut cs = drive_until(io, &mut out, 1_000, |v| v.len() == 4).unwrap();
    cs.sort_by_key(|c| c.token().get());
    assert_eq!(cs[0].result(), -ENOENT);
    assert_eq!(cs[1].result(), 0);
    assert_eq!(cs[2].result(), 0);
    assert_eq!(cs[3].result(), 0);

    io.submit(
        Token::new(5),
        Op::FileClose {
            slot: FileSlot::new(1),
        },
    )
    .unwrap();
    let closed = drive_until(io, &mut out, 1_000, |v| !v.is_empty()).unwrap();
    assert_eq!(closed[0].result(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f08_1_uring_batch_eight_mixed_ops_consumed_and_completed() {
    let dir = TempDir::new("f08_batch8").unwrap();
    let mut io = UringIo::new(file_limits(16)).expect("uring");
    let mut out = Completions::new(16);

    for i in 0..4u32 {
        let p = dir.file_path(&format!("f{i}.bin"));
        let plen = write_nul_path(&mut io, BufId::new(i), 0, p.to_str().unwrap());
        io.submit(
            Token::new(10 + u64::from(i)),
            Op::FileOpen {
                slot: FileSlot::new(i),
                path_buf: BufId::new(i),
                path_off: 0,
                path_len: plen,
                flags: open_flags(true),
            },
        )
        .unwrap();
        io.submit(Token::new(20 + u64::from(i)), Op::Timeout { nanos: 0 })
            .unwrap();
    }

    let cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 8).unwrap();
    assert!(cs.iter().all(|c| c.result() == 0), "unexpected: {cs:?}");

    for i in 0..4u32 {
        io.submit(
            Token::new(30 + u64::from(i)),
            Op::FileClose {
                slot: FileSlot::new(i),
            },
        )
        .unwrap();
    }
    let closed = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 4).unwrap();
    assert!(closed.iter().all(|c| c.result() == 0));
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f08_2_uring_submit_all_refused_open_does_not_block_trailing_ops() {
    let dir = TempDir::new("f08_refused").unwrap();
    let mut io = UringIo::new(file_limits(16)).expect("uring");
    let mut out = Completions::new(16);

    // Pre-open FileSlot(2) for a trailing FileWrite.
    let pre = dir.file_path("preopened.dat");
    let plen = write_nul_path(&mut io, BufId::new(2), 0, pre.to_str().unwrap());
    io.submit(
        Token::new(1),
        Op::FileOpen {
            slot: FileSlot::new(2),
            path_buf: BufId::new(2),
            path_off: 0,
            path_len: plen,
            flags: open_flags(true),
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 1_000, |v| !v.is_empty()).unwrap();

    // Queue failing open ahead of Timeout and FileWrite in one turn.
    let missing = dir.file_path("absent.dat");
    let mlen = write_nul_path(&mut io, BufId::new(0), 0, missing.to_str().unwrap());
    write_pool_bytes(&mut io, BufId::new(3), 0, b"trailing-write");
    io.submit(
        Token::new(10),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: mlen,
            flags: open_flags(false),
        },
    )
    .unwrap();
    io.submit(Token::new(11), Op::Timeout { nanos: 0 }).unwrap();
    io.submit(
        Token::new(12),
        Op::FileWrite {
            slot: FileSlot::new(2),
            buf: BufId::new(3),
            off: 0,
            len: 14,
            file_off: 0,
        },
    )
    .unwrap();

    let mut cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 3).unwrap();
    cs.sort_by_key(|c| c.token().get());
    assert_eq!(cs[0].result(), -ENOENT);
    assert_eq!(cs[1].result(), 0);
    assert_eq!(cs[2].result(), 14);
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f08_3_uring_multi_sqe_turns_return_ok_with_exact_consumed_count() {
    let mut io = UringIo::new(file_limits(16)).expect("uring");
    let mut out = Completions::new(16);
    for count in [6u64, 10, 4] {
        for i in 0..count {
            io.submit(Token::new(100 * count + i), Op::Timeout { nanos: 0 })
                .unwrap();
        }
        let cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == count as usize).unwrap();
        assert!(cs.iter().all(|c| c.result() == 0));
    }
}

/// Accepts `Sock(1)` from a `Sock(2)` connect and opens `mixed.dat` in `FileSlot(0)`, all in one
/// batch.
#[cfg(target_os = "linux")]
fn connect_and_open_together(io: &mut UringIo, out: &mut Completions, dir: &TempDir) {
    let bound = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(2),
            addr: bound,
        },
    )
    .unwrap();
    let fpath = dir.file_path("mixed.dat");
    let flen = write_nul_path(io, BufId::new(0), 0, fpath.to_str().unwrap());
    io.submit(
        Token::new(3),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: flen,
            flags: open_flags(true),
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(io, out, 1_000, |v| v.len() == 3).unwrap();
}

/// Submits a socket send and receive next to a file write and sync (tokens 10..=13).
#[cfg(target_os = "linux")]
fn submit_mixed_batch(io: &mut UringIo) {
    write_pool_bytes(io, BufId::new(1), 0, b"sock-msg");
    write_pool_bytes(io, BufId::new(2), 0, b"file-msg");
    let send = Op::Send {
        sock: Sock::new(2),
        buf: BufId::new(1),
        off: 0,
        len: 8,
        zero_copy: false,
    };
    let recv = Op::Recv {
        sock: Sock::new(1),
        buf: BufId::new(3),
        off: 0,
        len: 64,
    };
    let write = Op::FileWrite {
        slot: FileSlot::new(0),
        buf: BufId::new(2),
        off: 0,
        len: 8,
        file_off: 0,
    };
    let sync = Op::FileSync {
        slot: FileSlot::new(0),
        datasync: true,
    };
    for (tok, op) in [(10, send), (11, recv), (12, write), (13, sync)] {
        io.submit(Token::new(tok), op).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f08_4_uring_mixed_socket_and_file_ops_single_turn() {
    let dir = TempDir::new("f08_mixed").unwrap();
    let mut io = UringIo::new(file_limits(16)).expect("uring");
    let mut out = Completions::new(16);
    connect_and_open_together(&mut io, &mut out, &dir);
    submit_mixed_batch(&mut io);

    let mut cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 4).unwrap();
    cs.sort_by_key(|c| c.token().get());
    assert_eq!(cs[0].result(), 8);
    assert_eq!(cs[1].result(), 8);
    assert_eq!(cs[2].result(), 8);
    assert_eq!(cs[3].result(), 0);
}

#[test]
fn t1_f08_5_cross_backend_failing_file_op_does_not_strand_trailing_ops() {
    #[cfg(feature = "sim")]
    {
        let mut io = SimIo::new(file_limits(16), 3, Faults::default());
        io.setup_disk(
            longshore::sim::DiskConfig::BASE
                .with_max_files(8)
                .with_max_dirs(4)
                .with_max_bytes_per_file(64 * 1024)
                .with_max_name_len(128)
                .with_write_log_entries(16)
                .with_write_log_bytes(32 * 1024)
                .with_read_error_sectors(4)
                .with_sector_size(512),
        );
        submit_failing_open_then_trailing(&mut io, Path::new("/"));
    }
    #[cfg(unix)]
    {
        let dir = TempDir::new("f08_posix").unwrap();
        let mut io = posix_backend(file_limits(16)).expect("posix");
        submit_failing_open_then_trailing(&mut io, dir.path());
    }
}
