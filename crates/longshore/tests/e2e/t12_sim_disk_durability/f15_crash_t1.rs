// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 15: Whole-machine `power_loss` recovery invariants.

use super::super::harness::{drive_until, read_pool_bytes, submit_and_wait_one, write_pool_bytes};
use super::helpers::{
    close_file, listen_accept_connect, make_disk_sim, open_file, read_file_vec, sync_file,
    sync_root_dir, write_file,
};
use longshore::io::{BufId, Completion, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token};
use longshore::sim::SimIo;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// `T1-F15-1`: Unsynced 4-sector write across seeds yields absent, whole, or sector-cut prefix.
#[test]
fn t1_f15_1_multi_sector_unsynced_write_three_outcomes_across_seeds() {
    let (mut saw_absent, mut saw_whole, mut saw_cut) = (false, false, false);
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };

    for seed in 0..64u64 {
        let mut sim = make_disk_sim(seed, 512, 32, 64 * 1024);
        let mut out = Completions::new(8);
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/torn.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x11; 2048]),
            2048
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);

        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x22; 2048]),
            2048
        );
        sim.power_loss();

        assert_eq!(
            open_file(&mut sim, &mut out, 20, FileSlot(0), "/torn.dat", ro),
            0
        );
        let got = read_file_vec(&mut sim, &mut out, 21, FileSlot(0), 0, 2048).expect("read");
        assert_eq!(got.len(), 2048);
        let prefix_new = got.iter().take_while(|&&b| b == 0x22).count();
        assert_eq!(
            prefix_new % 512,
            0,
            "seed {seed}: cut at non-sector {prefix_new}"
        );
        assert!(got[..prefix_new].iter().all(|&b| b == 0x22));
        assert!(got[prefix_new..].iter().all(|&b| b == 0x11));

        match prefix_new {
            0 => saw_absent = true,
            2048 => saw_whole = true,
            _ => saw_cut = true,
        }
        assert_eq!(close_file(&mut sim, &mut out, 22, FileSlot(0)), 0);
    }
    assert!(
        saw_absent && saw_whole && saw_cut,
        "expected all 3 outcomes across seeds"
    );
}

/// `T1-F15-2`: Single-sector unsynced write is atomic (wholly kept or wholly dropped).
#[test]
fn t1_f15_2_single_sector_write_atomic_whole_or_absent() {
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    for seed in 0..40u64 {
        let mut sim = make_disk_sim(seed, 512, 32, 64 * 1024);
        let mut out = Completions::new(8);
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/one_sec.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x33; 512]),
            512
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);

        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x44; 512]),
            512
        );
        sim.power_loss();

        assert_eq!(
            open_file(&mut sim, &mut out, 20, FileSlot(0), "/one_sec.dat", ro),
            0
        );
        let got = read_file_vec(&mut sim, &mut out, 21, FileSlot(0), 0, 512).expect("read");
        assert!(
            got.iter().all(|&b| b == 0x33) || got.iter().all(|&b| b == 0x44),
            "seed {seed}: single sector torn mid-sector"
        );
    }
}

/// `T1-F15-3`: Overlapping unsynced writes `W1` then `W2` settle sequentially at `power_loss()`.
#[test]
fn t1_f15_3_overlapping_unsynced_writes_sequential_settlement() {
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let ro = OpenFlags {
        read: true,
        ..Default::default()
    };
    for seed in 0..40u64 {
        let mut sim = make_disk_sim(seed, 512, 32, 64 * 1024);
        let mut out = Completions::new(8);
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/seq.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x00; 2048]),
            2048
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);

        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0xAA; 2048]),
            2048
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 11, FileSlot(0), 512, &[0xBB; 1024]),
            1024
        );
        sim.power_loss();

        assert_eq!(
            open_file(&mut sim, &mut out, 20, FileSlot(0), "/seq.dat", ro),
            0
        );
        let got = read_file_vec(&mut sim, &mut out, 21, FileSlot(0), 0, 2048).expect("read");
        for sec in 0..4 {
            let chunk = &got[sec * 512..(sec + 1) * 512];
            let first = chunk[0];
            assert!(
                chunk.iter().all(|&b| b == first),
                "sector {sec} must be uniform"
            );
            if sec == 0 || sec == 3 {
                assert!(first == 0x00 || first == 0xAA);
            } else {
                assert!(first == 0x00 || first == 0xAA || first == 0xBB);
            }
        }
    }
}

/// `T1-F15-4`: Same seed + ops + `power_loss()` produces bit-identical bytes and trace hash.
#[test]
fn t1_f15_4_same_seed_power_loss_bit_identical_recovery() {
    let run = || {
        let mut sim = make_disk_sim(777, 512, 32, 64 * 1024);
        let mut out = Completions::new(8);
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        };
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/det.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x11; 2048]),
            2048
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);
        assert_eq!(
            write_file(&mut sim, &mut out, 5, FileSlot(0), 0, &[0x22; 2048]),
            2048
        );
        sim.power_loss();
        assert_eq!(
            open_file(&mut sim, &mut out, 6, FileSlot(0), "/det.dat", rw),
            0
        );
        let got = read_file_vec(&mut sim, &mut out, 7, FileSlot(0), 0, 2048).unwrap();
        (got, sim.trace_hash())
    };
    let (b1, h1) = run();
    let (b2, h2) = run();
    assert_eq!(b1, b2);
    assert_eq!(h1, h2);
}

/// Sends 14 bytes from `Sock(2)` to `Sock(1)` and checks they arrive.
fn tcp_round_trip_after_crash(sim: &mut SimIo, out: &mut Completions) {
    write_pool_bytes(sim, BufId(0), 0, b"post-crash-tcp");
    sim.submit(
        Token(12),
        Op::Send {
            sock: Sock(2),
            buf: BufId(0),
            off: 0,
            len: 14,
            zero_copy: false,
        },
    )
    .unwrap();
    sim.submit(
        Token(13),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 14,
        },
    )
    .unwrap();
    let io_done = drive_until(sim, out, 100, |v| v.len() == 2).unwrap();
    assert!(io_done.iter().all(|c| c.result == 14));
    assert_eq!(read_pool_bytes(sim, BufId(1), 0, 14), b"post-crash-tcp");
}

/// `T1-F15-5`: Network and storage callers can be rebuilt on the same `SimIo` after `power_loss()`.
#[test]
fn t1_f15_5_rebuild_network_and_storage_after_power_loss() {
    let mut sim = make_disk_sim(55, 512, 32, 64 * 1024);
    let mut out = Completions::new(16);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9001);
    let _: Vec<Completion> = listen_accept_connect(&mut sim, &mut out, addr, 1);

    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 3, FileSlot(0), "/srv.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 4, FileSlot(0), 0, b"before-crash"),
        12
    );
    assert_eq!(sync_file(&mut sim, &mut out, 5, FileSlot(0), false), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 6, FileSlot(1)), 0);

    sim.power_loss();

    // Rebuild listener + connection + file handle on the same SimIo.
    let conn = listen_accept_connect(&mut sim, &mut out, addr, 10);
    assert!(conn.iter().all(|c| c.result == 0));
    tcp_round_trip_after_crash(&mut sim, &mut out);

    assert_eq!(
        open_file(&mut sim, &mut out, 20, FileSlot(0), "/srv.dat", rw),
        0
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 21, FileSlot(0), 0, 12).unwrap(),
        b"before-crash"
    );
    let _: Completion = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(22),
        Op::Close { sock: Sock(1) },
        100,
    )
    .unwrap();
}
