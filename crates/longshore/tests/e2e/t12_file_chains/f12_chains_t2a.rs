// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 12 (Part A): Atomic `Busy` and short-write chain abort.

use super::super::harness::{drive_until, write_pool_bytes};
use super::helpers::{
    chain_limits, close_slot, open_slot, run_on_backends, submit_chain_and_collect, DynIo,
};
#[cfg(feature = "sim")]
use longshore::io::ECANCELED;
use longshore::io::{BufId, Busy, Completions, FileSlot, Io, Op, OpenFlags, Token};

/// `T2-F12-1`: Chain of length `N` when only `N - 1` ops are free returns `Err(Busy)` atomically.
#[test]
fn t2_f12_1_chain_atomic_busy_when_insufficient_ops_max() {
    run_on_backends(chain_limits(3), |io, prefix| {
        let mut out = Completions::new(8);
        let path = format!("{prefix}busy_chain.bin");
        let rw = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true);
        assert_eq!(
            open_slot(io, &mut out, 1, FileSlot::new(0), BufId::new(0), &path, rw),
            0
        );

        let mut d = DynIo(io);
        // Occupy 2 of 3 ops_max slots, leaving only 1 free slot for a 2-op chain.
        d.submit(Token::new(10), Op::Timeout { nanos: 100_000 })
            .unwrap();
        d.submit(Token::new(11), Op::Timeout { nanos: 100_000 })
            .unwrap();
        let chain = [
            (
                Token::new(20),
                Op::FileWrite {
                    slot: FileSlot::new(0),
                    buf: BufId::new(1),
                    off: 0,
                    len: 8,
                    file_off: 0,
                },
            ),
            (
                Token::new(21),
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: true,
                },
            ),
        ];
        assert_eq!(d.submit_chain(&chain), Err(Busy));

        let timers = drive_until(&mut d, &mut out, 10_000, |v| {
            v.iter().any(|c| c.token().get() == 10) && v.iter().any(|c| c.token().get() == 11)
        })
        .expect("timers");
        assert_eq!(timers.len(), 2, "Busy chain must not emit any completions");

        // Now all 3 slots are free: resubmitting the 2-op chain succeeds.
        write_pool_bytes(&mut d, BufId::new(1), 0, b"accepted");
        let cs = submit_chain_and_collect(io, &mut out, &chain);
        assert_eq!(
            cs.iter().find(|c| c.token().get() == 20).unwrap().result(),
            8
        );
        assert_eq!(
            cs.iter().find(|c| c.token().get() == 21).unwrap().result(),
            0
        );
        assert_eq!(close_slot(io, &mut out, 30, FileSlot::new(0)), 0);
    });
}

/// `T2-F12-2`: Short write in `[FileWrite, FileSync]` ends chain and cancels `FileSync`.
#[cfg(feature = "sim")]
#[test]
fn t2_f12_2_chain_short_write_cancels_sync() {
    let limits = chain_limits(16);
    let mut sim = super::helpers::make_sim(limits, 42);
    let mut out = Completions::new(8);
    let rw = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    assert_eq!(
        open_slot(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            BufId::new(0),
            "/short.wal",
            rw
        ),
        0
    );

    sim.set_disk_faults(longshore::sim::DiskFaults::NONE.with_short_write(1_000_000));
    sim.buf(BufId::new(0))[..4096].fill(0xCD);
    let cs = submit_chain_and_collect(
        &mut sim,
        &mut out,
        &[
            (
                Token::new(10),
                Op::FileWrite {
                    slot: FileSlot::new(0),
                    buf: BufId::new(0),
                    off: 0,
                    len: 4096,
                    file_off: 0,
                },
            ),
            (
                Token::new(11),
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: true,
                },
            ),
        ],
    );
    let w = cs.iter().find(|c| c.token().get() == 10).unwrap().result();
    let s = cs.iter().find(|c| c.token().get() == 11).unwrap().result();
    assert!(w > 0 && w < 4096, "expected short write count, got {w}");
    assert_eq!(w % 512, 0, "short write must cut on sector boundary");
    assert_eq!(s, -ECANCELED, "sync after short write must be cancelled");
    assert_eq!(close_slot(&mut sim, &mut out, 12, FileSlot::new(0)), 0);
}
