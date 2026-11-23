// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 12 (Part B): Position failures, shared-check refusals, and validation.

use super::super::harness::{drive_until, submit_and_wait_one, write_nul_path, write_pool_bytes};
use super::helpers::{
    chain_limits, close_slot, open_slot, run_on_backends, submit_chain_and_collect, DynIo,
    LINUX_EINVAL,
};
use longshore::io::{
    BufId, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token, ECANCELED, ENOENT,
};

/// Submits a three-op chain under tokens `base..base + 3` and returns the first three results
/// in completion order.
fn chain_results(io: &mut dyn Io, out: &mut Completions, base: u64, ops: [Op; 3]) -> [i32; 3] {
    let chain = [
        (Token::new(base), ops[0]),
        (Token::new(base + 1), ops[1]),
        (Token::new(base + 2), ops[2]),
    ];
    let cs = submit_chain_and_collect(io, out, &chain);
    [cs[0].result(), cs[1].result(), cs[2].result()]
}

/// `T2-F12-3`: Failure at position 0, 1, or 2 in a 3-op chain cancels only subsequent members.
#[test]
fn t2_f12_3_chain_failure_at_each_position_cancels_tail() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let ok_path = format!("{prefix}chain_pos_ok.bin");
        let missing = format!("{prefix}chain_pos_missing.bin");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        let ro = OpenFlags::NONE.with_read(true);
        assert_eq!(
            open_slot(
                io,
                &mut out,
                1,
                FileSlot::new(0),
                BufId::new(0),
                &ok_path,
                rw
            ),
            0
        );

        let mut d = DynIo(io);
        let mlen = write_nul_path(&mut d, BufId::new(0), 0, &missing);
        write_pool_bytes(&mut d, BufId::new(1), 0, b"before-fail");

        let fail_open = Op::FileOpen {
            slot: FileSlot::new(1),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: mlen,
            flags: ro,
        };
        let write_ok = Op::FileWrite {
            slot: FileSlot::new(0),
            buf: BufId::new(1),
            off: 0,
            len: 11,
            file_off: 0,
        };
        let sync_ok = Op::FileSync {
            slot: FileSlot::new(0),
            datasync: true,
        };

        // Position 0 fails: pos 1 and pos 2 are cancelled.
        let c0 = chain_results(io, &mut out, 10, [fail_open, write_ok, sync_ok]);
        assert_eq!(c0, [-ENOENT, -ECANCELED, -ECANCELED]);

        // Position 1 fails: pos 0 succeeds, pos 2 is cancelled.
        let c1 = chain_results(io, &mut out, 20, [write_ok, fail_open, sync_ok]);
        assert_eq!(c1, [11, -ENOENT, -ECANCELED]);

        // Position 2 fails: pos 0 and pos 1 succeed.
        let c2 = chain_results(io, &mut out, 30, [write_ok, sync_ok, fail_open]);
        assert_eq!(c2, [11, 0, -ENOENT]);
        assert_eq!(close_slot(io, &mut out, 40, FileSlot::new(0)), 0);
    });
}

/// Pos 0 opens with `direct = true`; pos 1 fails the shared check (unaligned len 100); pos 2 syncs.
fn direct_open_unaligned_write_sync(plen: u32) -> [(Token, Op); 3] {
    [
        (
            Token::new(1),
            Op::FileOpen {
                slot: FileSlot::new(0),
                path_buf: BufId::new(0),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags::NONE
                    .with_read(true)
                    .with_write(true)
                    .with_create(true)
                    .with_direct(true),
            },
        ),
        (
            Token::new(2),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 100,
                file_off: 0,
            },
        ),
        (
            Token::new(3),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ]
}

/// `T2-F12-4`: Shared-check refusal inside chain cancels EVERY other member without file mutation.
#[test]
fn t2_f12_4_chain_shared_check_refusal_cancels_all_without_mutation() {
    run_on_backends(chain_limits(16), |io, prefix| {
        let mut out = Completions::new(16);
        let never_created = format!("{prefix}must_not_exist.bin");
        let mut d = DynIo(io);
        let plen = write_nul_path(&mut d, BufId::new(0), 0, &never_created);

        let cs = submit_chain_and_collect(io, &mut out, &direct_open_unaligned_write_sync(plen));
        let result_of = |t: u64| {
            cs.iter()
                .find(|c| c.token() == Token::new(t))
                .unwrap()
                .result()
        };
        assert_eq!(result_of(1), -ECANCELED);
        assert_eq!(result_of(2), -LINUX_EINVAL);
        assert_eq!(result_of(3), -ECANCELED);

        d = DynIo(io);
        let st = submit_and_wait_one(
            &mut d,
            &mut out,
            Token::new(4),
            Op::FileStatus {
                path_buf: BufId::new(0),
                path_off: 0,
                path_len: plen,
                out_buf: BufId::new(1),
                out_off: 0,
            },
            10_000,
        )
        .expect("status");
        assert_eq!(st.result(), -ENOENT);
    });
}

/// `T2-F12-5`: Empty chain `&[]` and disallowed non-file ops in `submit_chain` are refused cleanly.
#[test]
fn t2_f12_5_chain_rejects_non_file_ops_and_empty_slice() {
    run_on_backends(chain_limits(4), |io, _| {
        let mut out = Completions::new(8);
        let empty_outcome = {
            let mut d = DynIo(io);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.submit_chain(&[])))
        };
        if let Ok(Ok(())) = empty_outcome {
            io.tick(false, &mut out).expect("tick empty");
            assert!(out.is_empty());
        }

        for bad_op in [
            Op::Timeout { nanos: 1_000 },
            Op::Close { sock: Sock::new(0) },
        ] {
            let res = {
                let mut d = DynIo(io);
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    d.submit_chain(&[(Token::new(50), bad_op)])
                }))
            };
            if let Ok(Ok(())) = res {
                let c = drive_until(&mut DynIo(io), &mut out, 100, |v| {
                    v.iter().any(|x| x.token().get() == 50)
                })
                .expect("ready completion");
                assert_eq!(c[0].result(), -LINUX_EINVAL);
            }
        }

        let mut d = DynIo(io);
        for t in 1..=4 {
            d.submit(Token::new(100 + t), Op::Timeout { nanos: 0 })
                .unwrap();
        }
        let done = drive_until(&mut d, &mut out, 100, |v| v.len() == 4).expect("4 timers");
        assert!(done.iter().all(|c| c.result() == 0));
    });
}
