// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 15: `power_loss` slot/timer/waker/read-error/op-counter corner cases.

use super::super::harness::{drive_until, submit_and_wait_one};
use super::helpers::{
    close_file, make_disk_sim, open_file, read_file_vec, sync_file, sync_root_dir, write_file,
    LINUX_EIO,
};
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token};
use longshore::sim::{DiskFaults, PlacedFault};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// `T2-F15-1`: `power_loss()` drops all in-flight ops with no completion, frees slots, clears timers.
#[test]
fn t2_f15_1_power_loss_drops_inflight_ops_frees_slots_clears_timers() {
    let mut sim = make_disk_sim(101, 512, 32, 64 * 1024);
    let mut out = Completions::new(16);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9100);
    sim.listen(Sock(0), addr).unwrap();
    sim.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .unwrap();
    sim.submit(
        Token(2),
        Op::Connect {
            sock: Sock(2),
            addr,
        },
    )
    .unwrap();
    let _ = drive_until(&mut sim, &mut out, 100, |v| v.len() == 2).unwrap();

    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 3, FileSlot(0), "/inf.dat", rw),
        0
    );

    // Queue in-flight Recv, Accept, Timeout, and FileWrite (before tick completes or delayed).
    sim.submit(
        Token(10),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    sim.submit(
        Token(11),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(3),
        },
    )
    .unwrap();
    sim.submit(Token(12), Op::Timeout { nanos: 1_000_000 })
        .unwrap();
    sim.submit(
        Token(13),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: BufId(1),
            off: 0,
            len: 512,
            file_off: 0,
        },
    )
    .unwrap();

    sim.power_loss();
    sim.tick(false, &mut out).expect("tick after crash");
    assert!(
        out.is_empty(),
        "power_loss must drop all in-flight ops with no completions"
    );

    // Socket and file slots are immediately free for reuse.
    sim.listen(Sock(0), addr).expect("listener slot 0 freed");
    assert_eq!(
        open_file(&mut sim, &mut out, 20, FileSlot(0), "/reused.dat", rw),
        0
    );
    assert_eq!(close_file(&mut sim, &mut out, 21, FileSlot(0)), 0);
}

/// `T2-F15-2`: `power_loss()` clears any pre-signalled waker flag.
#[test]
fn t2_f15_2_power_loss_clears_waker_flag() {
    let mut sim = make_disk_sim(102, 512, 16, 16_384);
    let mut out = Completions::new(8);
    let waker = sim.waker().expect("waker");
    waker.wake();

    sim.power_loss();

    sim.submit(Token(1), Op::AwaitWake).unwrap();
    sim.submit(Token(2), Op::Timeout { nanos: 1_000_000 })
        .unwrap();
    let c = drive_until(&mut sim, &mut out, 100, |v| !v.is_empty()).unwrap();
    assert_eq!(
        c[0].token,
        Token(2),
        "AwaitWake must NOT fire spuriously after power_loss cleared the waker flag"
    );
    waker.wake();
    let c2 = drive_until(&mut sim, &mut out, 100, |v| {
        v.iter().any(|x| x.token == Token(1))
    })
    .unwrap();
    assert_eq!(c2.iter().find(|x| x.token == Token(1)).unwrap().result, 0);
}

/// `T2-F15-3`: Persistent sector `read_error` survives `power_loss()` until covered by a full write.
#[test]
fn t2_f15_3_power_loss_preserves_sector_read_errors() {
    let mut sim = make_disk_sim(103, 512, 32, 64 * 1024);
    let mut out = Completions::new(8);
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/rerr.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x55; 512]),
        512
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);

    sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 10, FileSlot(0), 0, 512),
        Err(-LINUX_EIO)
    );
    sim.set_disk_faults(DiskFaults::default());

    sim.power_loss();

    assert_eq!(
        open_file(&mut sim, &mut out, 20, FileSlot(0), "/rerr.dat", rw),
        0
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 21, FileSlot(0), 0, 512),
        Err(-LINUX_EIO),
        "sector read error must persist across power_loss"
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 22, FileSlot(0), 0, &[0x66; 512]),
        512
    );
    assert_eq!(
        read_file_vec(&mut sim, &mut out, 23, FileSlot(0), 0, 512).unwrap(),
        vec![0x66; 512]
    );
    assert_eq!(close_file(&mut sim, &mut out, 24, FileSlot(0)), 0);
}

/// `T2-F15-4`: File op numbers for `PlacedFault` carry across `power_loss()`.
#[test]
fn t2_f15_4_power_loss_preserves_op_numbers_for_placed_faults() {
    let mut sim = make_disk_sim(104, 512, 32, 64 * 1024);
    let mut out = Completions::new(8);
    sim.place_disk_fault(7, PlacedFault::Fail);

    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    // Ops 1..=5 before power_loss: Open(1), Write(2), Sync(3), OpenDir(4), SyncDir(5), CloseDir(6) -> 6 ops!
    assert_eq!(
        open_file(&mut sim, &mut out, 1, FileSlot(0), "/opnum.dat", rw),
        0
    );
    assert_eq!(
        write_file(&mut sim, &mut out, 2, FileSlot(0), 0, b"before"),
        6
    );
    assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
    assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);

    sim.power_loss();

    // Op #7 after crash (first file op post-crash) must receive the placed fault!
    let r7 = open_file(&mut sim, &mut out, 10, FileSlot(0), "/opnum.dat", rw);
    assert_eq!(
        r7, -LINUX_EIO,
        "placed fault at op #7 must survive power_loss"
    );
    // Op #8 succeeds normally.
    assert_eq!(
        open_file(&mut sim, &mut out, 11, FileSlot(0), "/opnum.dat", rw),
        0
    );
    assert_eq!(close_file(&mut sim, &mut out, 12, FileSlot(0)), 0);
}

/// `T2-F15-5`: `power_loss()` cuts unsynced writes strictly on `sector_size = 4096` boundaries.
#[test]
fn t2_f15_5_power_loss_respects_4096_sector_cuts() {
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
        let mut sim = make_disk_sim(seed, 4096, 32, 128 * 1024);
        let mut out = Completions::new(8);
        assert_eq!(
            open_file(&mut sim, &mut out, 1, FileSlot(0), "/s4k.dat", rw),
            0
        );
        assert_eq!(
            write_file(&mut sim, &mut out, 2, FileSlot(0), 0, &[0x11; 16_384]),
            16_384
        );
        assert_eq!(sync_file(&mut sim, &mut out, 3, FileSlot(0), false), 0);
        assert_eq!(sync_root_dir(&mut sim, &mut out, 4, FileSlot(1)), 0);

        assert_eq!(
            write_file(&mut sim, &mut out, 10, FileSlot(0), 0, &[0x22; 16_384]),
            16_384
        );
        sim.power_loss();

        assert_eq!(
            open_file(&mut sim, &mut out, 20, FileSlot(0), "/s4k.dat", ro),
            0
        );
        let got = read_file_vec(&mut sim, &mut out, 21, FileSlot(0), 0, 16_384).expect("read");
        let prefix_new = got.iter().take_while(|&&b| b == 0x22).count();
        assert_eq!(
            prefix_new % 4096,
            0,
            "seed {seed}: non-4096 cut at {prefix_new}"
        );
        assert!(got[..prefix_new].iter().all(|&b| b == 0x22));
        assert!(got[prefix_new..].iter().all(|&b| b == 0x11));
        let _ = submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(22),
            Op::FileClose { slot: FileSlot(0) },
            100,
        );
    }
}
