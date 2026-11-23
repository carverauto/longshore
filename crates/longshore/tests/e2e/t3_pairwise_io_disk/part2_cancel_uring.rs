// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-03`–`T3-05`: chain cancellation,
//! `IORING_SETUP_SUBMIT_ALL` + ready list, and direct-I/O whole-chain refusal.

#[cfg(target_os = "linux")]
use super::super::harness::TempDir;
use super::super::harness::{drive_until, write_pool_bytes};
use super::{
    disk_config, file_limits, open_at, read_only, result_of, run_op, rw_create, sync_op, write_op,
    write_path,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, Token, ECANCELED, EINVAL, ENOENT};
use longshore::sim::{Faults, SimIo};

/// Opens, writes and syncs `/t3_03.dat` as one chain and cancels the trailing
/// sync: the cancel must miss (the sync already belongs to a running chain)
/// and the sync must succeed.
fn cancel_trailing_member_misses(sim: &mut SimIo, out: &mut Completions) {
    let plen = write_path(sim, BufId::new(4), "/t3_03.dat");
    write_pool_bytes(sim, BufId::new(0), 0, &[0xAB; 4096]);
    let open = Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf: BufId::new(4),
        path_off: 0,
        path_len: plen,
        flags: rw_create(),
    };
    let chain = [
        (Token::new(100), open),
        (
            Token::new(101),
            write_op(FileSlot::new(0), BufId::new(0), 4096, 0),
        ),
        (Token::new(102), sync_op(FileSlot::new(0), true)),
    ];
    sim.submit_chain(&chain).unwrap();
    sim.submit(
        Token::new(200),
        Op::Cancel {
            target: Token::new(102),
        },
    )
    .unwrap();
    let cs = drive_until(sim, out, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 200) && v.iter().any(|c| c.token().get() == 102)
    })
    .unwrap();
    assert!(result_of(&cs, 200) < 0);
    assert_eq!(result_of(&cs, 102), 0);
}

/// Writes and syncs as a chain and cancels the head: if the cancel lands,
/// head and tail both end `ECANCELED`.
fn cancel_head_cancels_whole_chain(sim: &mut SimIo, out: &mut Completions) {
    let chain2 = [
        (
            Token::new(110),
            write_op(FileSlot::new(0), BufId::new(0), 4096, 4096),
        ),
        (Token::new(111), sync_op(FileSlot::new(0), true)),
    ];
    sim.submit_chain(&chain2).unwrap();
    sim.submit(
        Token::new(201),
        Op::Cancel {
            target: Token::new(110),
        },
    )
    .unwrap();
    let cs2 = drive_until(sim, out, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 110)
            && v.iter().any(|c| c.token().get() == 111)
            && v.iter().any(|c| c.token().get() == 201)
    })
    .unwrap();
    let head = result_of(&cs2, 110);
    let tail = result_of(&cs2, 111);
    if head < 0 {
        assert_eq!(head, -ECANCELED);
        assert_eq!(tail, -ECANCELED);
    }
}

#[test]
fn t3_03_cancel_running_head_vs_trailing_chain_member() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 303, Faults::NONE.with_delay(500_000));
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    cancel_trailing_member_misses(&mut sim, &mut out);
    cancel_head_cancels_whole_chain(&mut sim, &mut out);
    assert_eq!(
        run_op(
            &mut sim,
            &mut out,
            120,
            Op::FileClose {
                slot: FileSlot::new(0)
            },
            10_000
        ),
        0
    );
    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            121,
            FileSlot::new(0),
            "/t3_03.dat",
            read_only()
        ),
        0
    );
}

#[cfg(target_os = "linux")]
#[test]
fn t3_04_uring_submit_all_ready_list_and_valid_chain() {
    let dir = TempDir::new("t3_04").unwrap();
    let file_p = dir.file_path("ok.dat");
    let limits = file_limits(4, 2, 4096);
    let mut io = longshore::io::uring::UringIo::new(limits).unwrap();
    let mut out = Completions::new(16);

    let direct = rw_create().with_direct(true);
    assert_eq!(
        open_at(
            &mut io,
            &mut out,
            1,
            FileSlot::new(0),
            file_p.to_str().unwrap(),
            direct
        ),
        0
    );

    let missing_len = write_path(
        &mut io,
        BufId::new(5),
        dir.file_path("nope").to_str().unwrap(),
    );
    write_pool_bytes(&mut io, BufId::new(8), 0, &[0x5A; 4096]);
    io.submit(
        Token::new(10),
        Op::FileStatus {
            path_buf: BufId::new(5),
            path_off: 0,
            path_len: missing_len,
            out_buf: BufId::new(6),
            out_off: 0,
        },
    )
    .unwrap();
    io.submit_chain(&[
        (
            Token::new(11),
            write_op(FileSlot::new(0), BufId::new(8), 4096, 0),
        ),
        (Token::new(12), sync_op(FileSlot::new(0), true)),
    ])
    .unwrap();
    io.submit(
        Token::new(13),
        write_op(FileSlot::new(0), BufId::new(0), 17, 0),
    )
    .unwrap();

    let cs = drive_until(&mut io, &mut out, 10_000, |v| {
        [10, 11, 12, 13]
            .iter()
            .all(|t| v.iter().any(|c| c.token() == Token::new(*t)))
    })
    .unwrap();
    assert_eq!(result_of(&cs, 10), -ENOENT);
    assert_eq!(result_of(&cs, 11), 4096);
    assert_eq!(result_of(&cs, 12), 0);
    assert_eq!(result_of(&cs, 13), -EINVAL);
}

#[test]
fn t3_05_direct_open_and_unaligned_write_chain_whole_refusal() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 305, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    let plen = write_path(&mut sim, BufId::new(4), "/t3_05_never_created.dat");
    write_pool_bytes(&mut sim, BufId::new(0), 0, &[0xCC; 128]);
    let open = Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf: BufId::new(4),
        path_off: 0,
        path_len: plen,
        flags: rw_create().with_direct(true),
    };
    let chain = [
        (Token::new(1), open),
        (
            Token::new(2),
            write_op(FileSlot::new(0), BufId::new(0), 100, 0),
        ),
        (Token::new(3), sync_op(FileSlot::new(0), true)),
    ];
    sim.submit_chain(&chain).unwrap();
    let cs = drive_until(&mut sim, &mut out, 100, |v| v.len() == 3).unwrap();
    assert_eq!(result_of(&cs, 1), -ECANCELED);
    assert_eq!(result_of(&cs, 2), -EINVAL);
    assert_eq!(result_of(&cs, 3), -ECANCELED);

    let status = Op::FileStatus {
        path_buf: BufId::new(4),
        path_off: 0,
        path_len: plen,
        out_buf: BufId::new(5),
        out_off: 0,
    };
    assert_eq!(run_op(&mut sim, &mut out, 4, status, 100), -ENOENT);
    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            5,
            FileSlot::new(0),
            "/t3_05_after.dat",
            rw_create()
        ),
        0
    );
}
