// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 10: `FileSlot`, `OpenFlags`, `io::files`, and ready list.

use super::super::harness::{
    drive_until_nonblocking, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{
    close_slot, file_limits, open_slot, run_on_backends, run_op, rw_create, DynIo, LINUX_EINVAL,
};
use longshore::io::{BufId, Completion, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// `T1-F10-1`: Open into `FileSlot(0)`, write, close, and reopen another file into `FileSlot(0)`.
#[test]
fn t1_f10_1_open_write_close_reopen_same_slot() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let p1 = format!("{prefix}slot0_first.dat");
        let p2 = format!("{prefix}slot0_second.dat");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot::new(0), BufId::new(0), &p1, rw),
            0
        );

        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId::new(1), 0, b"alpha-slot");
        let wr = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(2),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 10,
                file_off: 0,
            },
            10_000,
        )
        .expect("write");
        assert_eq!(wr.result(), 10);
        assert_eq!(close_slot(io, &mut out, 3, FileSlot::new(0)), 0);

        // Reuse FileSlot(0) immediately for a second distinct file.
        assert_eq!(
            open_slot(io, &mut out, 4, FileSlot::new(0), BufId::new(0), &p2, rw),
            0
        );
        dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId::new(1), 0, b"beta!");
        let wr2 = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(5),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 5,
                file_off: 0,
            },
            10_000,
        )
        .expect("write2");
        assert_eq!(wr2.result(), 5);
        assert_eq!(close_slot(io, &mut out, 6, FileSlot::new(0)), 0);
    });
}

/// `T1-F10-2`: Reopening an existing non-empty file with `empty: true` truncates it to size 0.
#[test]
fn t1_f10_2_open_empty_truncates_existing_file_to_zero() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}truncate_me.dat");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        assert_eq!(
            open_slot(io, &mut out, 1, SLOT_0, BufId::new(0), &path, rw),
            0
        );
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId::new(1), 0, b"non-empty-payload");
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token::new(2),
                Op::FileWrite {
                    slot: SLOT_0,
                    buf: BufId::new(1),
                    off: 0,
                    len: 17,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result(),
            17
        );
        assert_eq!(close_slot(io, &mut out, 3, SLOT_0), 0);

        let empty_flags = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_empty(true);
        assert_eq!(
            open_slot(io, &mut out, 4, SLOT_0, BufId::new(0), &path, empty_flags),
            0
        );
        assert_eq!(close_slot(io, &mut out, 5, SLOT_0), 0);

        dyn_io = DynIo(io);
        let path_len = write_nul_path(&mut dyn_io, BufId::new(0), 0, &path);
        let st = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(6),
            Op::FileStatus {
                path_buf: BufId::new(0),
                path_off: 0,
                path_len,
                out_buf: BufId::new(1),
                out_off: 0,
            },
            10_000,
        )
        .expect("status");
        assert_eq!(st.result(), 0);
        assert_eq!(
            longshore::io::files::file_size(dyn_io.buf(BufId::new(1))),
            0
        );
    });
}

/// `T1-F10-3`: `create | exclusive | read | write` creates new file, then `empty` clears it.
#[test]
fn t1_f10_3_open_create_exclusive_then_empty() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}excl_empty.dat");
        let excl = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true)
            .with_exclusive(true);
        assert_eq!(
            open_slot(io, &mut out, 1, SLOT_0, BufId::new(0), &path, excl),
            0
        );
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId::new(1), 0, b"created-exclusively");
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token::new(2),
                Op::FileWrite {
                    slot: SLOT_0,
                    buf: BufId::new(1),
                    off: 0,
                    len: 19,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result(),
            19
        );
        assert_eq!(close_slot(io, &mut out, 3, SLOT_0), 0);

        let empty = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_empty(true);
        assert_eq!(
            open_slot(io, &mut out, 4, SLOT_0, BufId::new(0), &path, empty),
            0
        );
        dyn_io = DynIo(io);
        let rd = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(5),
            Op::FileRead {
                slot: SLOT_0,
                buf: BufId::new(1),
                off: 0,
                len: 32,
                file_off: 0,
            },
            10_000,
        )
        .expect("read");
        assert_eq!(rd.result(), 0, "emptied file must read 0 bytes");
        assert_eq!(close_slot(io, &mut out, 6, SLOT_0), 0);
    });
}

/// `T1-F10-4`: Shared validator refusal populates ready list and `tick(true)` returns immediately.
#[test]
fn t1_f10_4_ready_list_drains_without_blocking() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, _| {
        let mut out = Completions::new(8);
        // Submit a long timer alongside a shared-check-refused read on an unopened FileSlot.
        io.submit(
            Token::new(1),
            Op::Timeout {
                nanos: 5_000_000_000,
            },
        )
        .expect("timer");
        io.submit(
            Token::new(2),
            Op::FileRead {
                slot: FileSlot::new(0),
                buf: BufId::new(0),
                off: 0,
                len: 16,
                file_off: 0,
            },
        )
        .expect("refused op accepted into ready list");

        let start = std::time::Instant::now();
        io.tick(true, &mut out).expect("tick");
        let elapsed = start.elapsed();
        let c = out
            .pop()
            .expect("ready-list completion must arrive immediately");
        assert_eq!(c.token(), Token::new(2));
        assert!(
            c.result() < 0,
            "expected negative refusal, got {}",
            c.result()
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "ready list must prevent kernel sleep behind 5s timer (took {elapsed:?})"
        );
        // Cancel the 5s timer so drop/teardown is instantaneous.
        io.submit(
            Token::new(3),
            Op::Cancel {
                target: Token::new(1),
            },
        )
        .expect("cancel");
        let mut dyn_io = DynIo(io);
        let _: Option<Vec<Completion>> = drive_until_nonblocking(&mut dyn_io, &mut out, 100, |v| {
            v.iter().any(|x| x.token().get() == 1)
        })
        .ok();
        let _: i32 = LINUX_EINVAL;
    });
}

/// Accepts `Sock(1)` from a `Sock(2)` loopback connect; both must succeed.
fn connect_loopback_pair(io: &mut dyn Io, out: &mut Completions) {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let bound = io.listen(Sock::new(0), addr).expect("listen");
    io.submit(
        Token::new(100),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .unwrap();
    io.submit(
        Token::new(101),
        Op::Connect {
            sock: Sock::new(2),
            addr: bound,
        },
    )
    .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 2 {
        io.tick(true, out).expect("tick");
        while let Some(c) = out.pop() {
            seen.push(c);
        }
    }
    assert!(seen.iter().all(|c| c.result() >= 0));
}

/// Writes and reads back seven bytes on `FileSlot(2)`.
fn round_trip_slot_2(io: &mut dyn Io, out: &mut Completions) {
    let mut dyn_io = DynIo(io);
    write_pool_bytes(&mut dyn_io, BufId::new(0), 0, b"coexist");
    let write = Op::FileWrite {
        slot: FileSlot::new(2),
        buf: BufId::new(0),
        off: 0,
        len: 7,
        file_off: 0,
    };
    assert_eq!(run_op(&mut dyn_io, out, 200, write, "write"), 7);
    let read = Op::FileRead {
        slot: FileSlot::new(2),
        buf: BufId::new(1),
        off: 0,
        len: 7,
        file_off: 0,
    };
    assert_eq!(run_op(&mut dyn_io, out, 201, read, "read"), 7);
    assert_eq!(
        read_pool_bytes(&mut dyn_io, BufId::new(1), 0, 7),
        b"coexist"
    );
}

/// `T1-F10-5`: Open files in `0..files_max` simultaneously alongside sockets in `0..sockets_max`.
#[test]
fn t1_f10_5_concurrent_file_and_socket_slots_independent() {
    let limits = file_limits(3, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(16);
        for s in 0..limits.files_max() {
            let p = format!("{prefix}multi_{s}.dat");
            let tok = 10 + u64::from(s);
            assert_eq!(
                open_slot(
                    io,
                    &mut out,
                    tok,
                    FileSlot::new(s),
                    BufId::new(0),
                    &p,
                    rw_create(false)
                ),
                0
            );
        }
        connect_loopback_pair(io, &mut out);

        // Write & read on FileSlot(2) while sockets 0, 1, 2 remain open.
        round_trip_slot_2(io, &mut out);

        for s in 0..limits.files_max() {
            assert_eq!(
                close_slot(io, &mut out, 300 + u64::from(s), FileSlot::new(s)),
                0
            );
        }
        for sock in [Sock::new(0), Sock::new(1), Sock::new(2)] {
            let _: Completion = submit_and_wait_one(
                &mut DynIo(io),
                &mut out,
                Token::new(400 + u64::from(sock.get())),
                Op::Close { sock },
                10_000,
            )
            .expect("close sock");
        }
    });
}
