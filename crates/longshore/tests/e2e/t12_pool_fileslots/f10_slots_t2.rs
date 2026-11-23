// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 10: `FileSlot` conflict, invalid paths, and shared-check refusals.

use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use super::helpers::{
    close_slot, file_limits, open_slot, run_on_backends, DynIo, LINUX_EBUSY, LINUX_EINVAL,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token, EBADF, ENOENT};

/// `T2-F10-1`: Opening into an already `Open` `FileSlot(0)` completes with `-EBUSY`.
#[test]
fn t2_f10_1_open_into_already_open_slot_returns_ebusy() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let p1 = format!("{prefix}held.dat");
        let p2 = format!("{prefix}intruder.dat");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot::new(0), BufId::new(0), &p1, rw),
            0
        );
        let mut dyn_io = DynIo(io);
        write_pool_bytes(&mut dyn_io, BufId::new(1), 0, b"keep-intact");
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token::new(2),
                Op::FileWrite {
                    slot: FileSlot::new(0),
                    buf: BufId::new(1),
                    off: 0,
                    len: 11,
                    file_off: 0,
                },
                10_000,
            )
            .expect("write")
            .result(),
            11
        );

        let dup = open_slot(io, &mut out, 3, FileSlot::new(0), BufId::new(0), &p2, rw);
        assert_eq!(dup, -LINUX_EBUSY);

        dyn_io = DynIo(io);
        assert_eq!(
            submit_and_wait_one(
                &mut dyn_io,
                &mut out,
                Token::new(4),
                Op::FileRead {
                    slot: FileSlot::new(0),
                    buf: BufId::new(1),
                    off: 16,
                    len: 11,
                    file_off: 0,
                },
                10_000,
            )
            .expect("read")
            .result(),
            11
        );
        assert_eq!(
            read_pool_bytes(&mut dyn_io, BufId::new(1), 16, 11),
            b"keep-intact"
        );
        assert_eq!(close_slot(io, &mut out, 5, FileSlot::new(0)), 0);
    });
}

/// `T2-F10-2`: Opening into a slot whose prior `FileOpen` is still `Opening` returns `-EBUSY`.
#[test]
fn t2_f10_2_open_into_opening_slot_returns_ebusy() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let p1 = format!("{prefix}opening_first.dat");
        let p2 = format!("{prefix}opening_second.dat");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        let mut dyn_io = DynIo(io);
        let l1 = write_nul_path(&mut dyn_io, BufId::new(0), 0, &p1);
        let l2 = write_nul_path(&mut dyn_io, BufId::new(1), 0, &p2);
        dyn_io
            .submit(
                Token::new(1),
                Op::FileOpen {
                    slot: FileSlot::new(0),
                    path_buf: BufId::new(0),
                    path_off: 0,
                    path_len: l1,
                    flags: rw,
                },
            )
            .unwrap();
        dyn_io
            .submit(
                Token::new(2),
                Op::FileOpen {
                    slot: FileSlot::new(0),
                    path_buf: BufId::new(1),
                    path_off: 0,
                    path_len: l2,
                    flags: rw,
                },
            )
            .unwrap();
        let cs = drive_until(&mut dyn_io, &mut out, 10_000, |v| {
            v.iter().any(|c| c.token().get() == 1) && v.iter().any(|c| c.token().get() == 2)
        })
        .expect("both completes");
        let r1 = cs.iter().find(|c| c.token().get() == 1).unwrap().result();
        let r2 = cs.iter().find(|c| c.token().get() == 2).unwrap().result();
        assert_eq!(r1, 0);
        assert_eq!(r2, -LINUX_EBUSY);
        assert_eq!(close_slot(io, &mut out, 3, FileSlot::new(0)), 0);
    });
}

/// `T2-F10-3`: Opening missing path without `create` fails with `-ENOENT` and frees the slot.
#[test]
fn t2_f10_3_open_missing_without_create_enoent_frees_slot() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let missing = format!("{prefix}does_not_exist.dat");
        let ro = OpenFlags::NONE.with_read(true);
        assert_eq!(
            open_slot(
                io,
                &mut out,
                1,
                FileSlot::new(0),
                BufId::new(0),
                &missing,
                ro
            ),
            -ENOENT
        );

        let created = format!("{prefix}now_exists.dat");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        assert_eq!(
            open_slot(
                io,
                &mut out,
                2,
                FileSlot::new(0),
                BufId::new(0),
                &created,
                rw
            ),
            0,
            "slot 0 must be Free again after failed open"
        );
        assert_eq!(close_slot(io, &mut out, 3, FileSlot::new(0)), 0);
    });
}

/// `T2-F10-4`: Unterminated path, embedded NUL, or empty/oversize path fails with `-EINVAL`.
#[test]
fn t2_f10_4_invalid_nul_paths_refused_einval() {
    let limits = file_limits(4, 0, 0, 4096);
    run_on_backends(limits, |io, prefix| {
        let mut out = Completions::new(8);
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);

        // 1. Missing trailing NUL byte.
        let mut dyn_io = DynIo(io);
        let raw = format!("{prefix}no_nul.dat");
        write_pool_bytes(&mut dyn_io, BufId::new(0), 0, raw.as_bytes());
        let r1 = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(1),
            Op::FileOpen {
                slot: FileSlot::new(0),
                path_buf: BufId::new(0),
                path_off: 0,
                path_len: raw.len() as u32,
                flags: rw,
            },
            10_000,
        )
        .expect("tick")
        .result();
        assert_eq!(r1, -LINUX_EINVAL);

        // 2. Embedded NUL before end.
        let mut bad = format!("{prefix}mid").into_bytes();
        bad.push(0);
        bad.extend_from_slice(b"tail\0");
        write_pool_bytes(&mut dyn_io, BufId::new(0), 0, &bad);
        let r2 = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(2),
            Op::FileOpen {
                slot: FileSlot::new(0),
                path_buf: BufId::new(0),
                path_off: 0,
                path_len: bad.len() as u32,
                flags: rw,
            },
            10_000,
        )
        .expect("tick")
        .result();
        assert_eq!(r2, -LINUX_EINVAL);

        // 3. Length exceeding pool buffer.
        let small = limits.small(0);
        let r3 = submit_and_wait_one(
            &mut dyn_io,
            &mut out,
            Token::new(3),
            Op::FileOpen {
                slot: FileSlot::new(0),
                path_buf: small,
                path_off: 0,
                path_len: limits.small_buffer_size() + 1,
                flags: rw,
            },
            10_000,
        )
        .expect("tick")
        .result();
        assert_eq!(r3, -LINUX_EINVAL);
    });
}

/// `T2-F10-5`: File ops on `Free` or out-of-range `FileSlot` are refused cleanly without stalling.
#[test]
fn t2_f10_5_ops_on_free_or_oob_slot_refused_via_ready_list() {
    let limits = file_limits(2, 0, 0, 4096);
    run_on_backends(limits, |io, _| {
        let mut out = Completions::new(16);
        let mut dyn_io = DynIo(io);
        let cases = [
            (
                10u64,
                Op::FileRead {
                    slot: FileSlot::new(0),
                    buf: BufId::new(0),
                    off: 0,
                    len: 64,
                    file_off: 0,
                },
            ),
            (
                11,
                Op::FileWrite {
                    slot: FileSlot::new(1),
                    buf: BufId::new(0),
                    off: 0,
                    len: 64,
                    file_off: 0,
                },
            ),
            (
                12,
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: false,
                },
            ),
            (
                13,
                Op::FileClose {
                    slot: FileSlot::new(0),
                },
            ),
            (
                14,
                Op::FileRead {
                    slot: FileSlot::new(limits.files_max()),
                    buf: BufId::new(0),
                    off: 0,
                    len: 64,
                    file_off: 0,
                },
            ),
        ];
        for (tok, op) in cases {
            let c = submit_and_wait_one(&mut dyn_io, &mut out, Token::new(tok), op, 1_000)
                .expect("must not stall");
            assert!(
                c.result() == -EBADF || c.result() == -LINUX_EINVAL,
                "token {tok} expected -EBADF or -EINVAL, got {}",
                c.result()
            );
        }
    });
}
