// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 10: `FileSlot`, `OpenFlags`, `io::files`, and ready list.

use super::super::harness::{
    drive_until_nonblocking, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{close_slot, file_limits, open_slot, run_on_backends, DynIo, LINUX_EINVAL};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// `T1-F10-1`: Open into `FileSlot(0)`, write, close, and reopen another file into `FileSlot(0)`.
#[test]
fn t1_f10_1_open_write_close_reopen_same_slot() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let p1 = format!("{prefix}slot0_first.dat");
        let p2 = format!("{prefix}slot0_second.dat");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &p1, rw),
            0
        );

        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId(1), 0, b"alpha-slot");
        let wr = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 10,
                file_off: 0,
            },
            10_000,
        )
        .expect("write");
        assert_eq!(wr.result, 10);
        assert_eq!(close_slot(io, &mut out, 3, FileSlot(0)), 0);

        // Reuse FileSlot(0) immediately for a second distinct file.
        assert_eq!(
            open_slot(io, &mut out, 4, FileSlot(0), BufId(0), &p2, rw),
            0
        );
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId(1), 0, b"beta!");
        let wr2 = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(5),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 5,
                file_off: 0,
            },
            10_000,
        )
        .expect("write2");
        assert_eq!(wr2.result, 5);
        assert_eq!(close_slot(io, &mut out, 6, FileSlot(0)), 0);
    });
}

/// `T1-F10-2`: Reopening an existing non-empty file with `empty: true` truncates it to size 0.
#[test]
fn t1_f10_2_open_empty_truncates_existing_file_to_zero() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}truncate_me.dat");
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, rw),
            0
        );
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId(1), 0, b"non-empty-payload");
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 17,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result,
            17
        );
        assert_eq!(close_slot(io, &mut out, 3, FileSlot(0)), 0);

        let empty_flags = OpenFlags {
            read: true,
            write: true,
            empty: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 4, FileSlot(0), BufId(0), &path, empty_flags),
            0
        );
        assert_eq!(close_slot(io, &mut out, 5, FileSlot(0)), 0);

        let mut dyn_io = DynIo(io);
        let path_len = write_nul_path(&mut dyn_io, BufId(0), 0, &path);
        let st = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(6),
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len,
                out_buf: BufId(1),
                out_off: 0,
            },
            10_000,
        )
        .expect("status");
        assert_eq!(st.result, 0);
        assert_eq!(longshore::io::files::file_size(dyn_io.buf(BufId(1))), 0);
    });
}

/// `T1-F10-3`: `create | exclusive | read | write` creates new file, then `empty` clears it.
#[test]
fn t1_f10_3_open_create_exclusive_then_empty() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}excl_empty.dat");
        let excl = OpenFlags {
            read: true,
            write: true,
            create: true,
            exclusive: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot(0), BufId(0), &path, excl),
            0
        );
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId(1), 0, b"created-exclusively");
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 19,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result,
            19
        );
        assert_eq!(close_slot(io, &mut out, 3, FileSlot(0)), 0);

        let empty = OpenFlags {
            read: true,
            write: true,
            empty: true,
            ..Default::default()
        };
        assert_eq!(
            open_slot(io, &mut out, 4, FileSlot(0), BufId(0), &path, empty),
            0
        );
        let mut dyn_io = DynIo(io);
        let rd = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token(5),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 32,
                file_off: 0,
            },
            10_000,
        )
        .expect("read");
        assert_eq!(rd.result, 0, "emptied file must read 0 bytes");
        assert_eq!(close_slot(io, &mut out, 6, FileSlot(0)), 0);
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
            Token(1),
            Op::Timeout {
                nanos: 5_000_000_000,
            },
        )
        .expect("timer");
        io.submit(
            Token(2),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(0),
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
        assert_eq!(c.token, Token(2));
        assert!(c.result < 0, "expected negative refusal, got {}", c.result);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "ready list must prevent kernel sleep behind 5s timer (took {elapsed:?})"
        );
        // Cancel the 5s timer so drop/teardown is instantaneous.
        io.submit(Token(3), Op::Cancel { target: Token(1) })
            .expect("cancel");
        let mut dyn_io = DynIo(io);
        let _ = drive_until_nonblocking(&mut dyn_io, &mut out, 100, |v| {
            v.iter().any(|x| x.token == Token(1))
        })
        .ok();
        let _ = LINUX_EINVAL;
    });
}

/// `T1-F10-5`: Open files in `0..files_max` simultaneously alongside sockets in `0..sockets_max`.
#[test]
fn t1_f10_5_concurrent_file_and_socket_slots_independent() {
    let limits = file_limits(3, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(16);
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        for s in 0..limits.files_max {
            let p = format!("{prefix}multi_{s}.dat");
            assert_eq!(
                open_slot(io, &mut out, 10 + s as u64, FileSlot(s), BufId(0), &p, rw),
                0
            );
        }

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let bound = io.listen(Sock(0), addr).expect("listen");
        io.submit(
            Token(100),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(1),
            },
        )
        .unwrap();
        io.submit(
            Token(101),
            Op::Connect {
                sock: Sock(2),
                addr: bound,
            },
        )
        .unwrap();
        let mut seen = Vec::new();
        while seen.len() < 2 {
            io.tick(true, &mut out).expect("tick");
            while let Some(c) = out.pop() {
                seen.push(c);
            }
        }
        assert!(seen.iter().all(|c| c.result >= 0));

        // Write & read on FileSlot(2) while sockets 0, 1, 2 remain open.
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId(0), 0, b"coexist");
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token(200),
                Op::FileWrite {
                    slot: FileSlot(2),
                    buf: BufId(0),
                    off: 0,
                    len: 7,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result,
            7
        );
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token(201),
                Op::FileRead {
                    slot: FileSlot(2),
                    buf: BufId(1),
                    off: 0,
                    len: 7,
                    file_off: 0,
                },
                10_000,
            )
            .expect("read")
            .result,
            7
        );
        assert_eq!(read_pool_bytes(&mut dyn_io, BufId(1), 0, 7), b"coexist");

        for s in 0..limits.files_max {
            assert_eq!(close_slot(io, &mut out, 300 + s as u64, FileSlot(s)), 0);
        }
        for sock in [Sock(0), Sock(1), Sock(2)] {
            let _ = submit_and_wait_one(
                &mut DynIo(io),
                &mut out,
                Token(400 + sock.0 as u64),
                Op::Close { sock },
                10_000,
            )
            .expect("close sock");
        }
    });
}
