// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-06`–`T3-08`: `SimIo` trace parity with zero
//! `DiskFaults`, `power_loss` + `PlacedFault` op continuity, and chained short write.

use super::super::harness::{drive_until, submit_and_wait_one, write_pool_bytes};
use super::{disk_config, file_limits, open_file};
use longshore::io::{
    BufId, Completion, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token, ECANCELED,
};
use longshore::sim::{DiskFaults, Faults, PlacedFault, SimIo};

const EIO: i32 = 5;

#[test]
fn t3_06_simio_trace_parity_with_zero_disk_faults() {
    let run_mixed_zero_faults = |seed: u64| {
        let limits = file_limits(4, 2, 4096);
        let mut sim = SimIo::new(limits, seed, Faults::default());
        sim.setup_disk(disk_config());
        sim.set_disk_faults(DiskFaults::default());
        let mut out = Completions::new(16);

        let addr = sim
            .listen(Sock(0), "192.0.2.36:9306".parse().unwrap())
            .unwrap();
        sim.submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
        sim.submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr,
            },
        )
        .unwrap();
        let _: Vec<Completion> = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();

        assert_eq!(
            open_file(
                &mut sim,
                &mut out,
                Token(3),
                FileSlot(0),
                BufId(4),
                "/t3_06.dat",
                OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    ..Default::default()
                }
            )
            .unwrap(),
            0
        );
        write_pool_bytes(&mut sim, BufId(0), 0, &[0x42; 512]);
        sim.submit_chain(&[
            (
                Token(4),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(0),
                    off: 0,
                    len: 512,
                    file_off: 0,
                },
            ),
            (
                Token(5),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let _: Vec<Completion> = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();
        (sim.trace_hash(), sim.disk_fault_counters())
    };
    let (h1, c1) = run_mixed_zero_faults(77);
    let (h2, c2) = run_mixed_zero_faults(77);
    assert_eq!(
        h1, h2,
        "same seed with DiskFaults=0 must give identical trace hash"
    );
    assert_eq!(c1, Default::default());
    assert_eq!(c2, Default::default());
}

#[test]
fn t3_07_simio_power_loss_and_placed_fault_op_number_continuity() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(
        limits,
        307,
        Faults {
            delay: 100_000,
            ..Default::default()
        },
    );
    sim.setup_disk(disk_config());
    sim.set_disk_faults(DiskFaults::default());
    let mut out = Completions::new(16);
    sim.place_disk_fault(6, PlacedFault::Fail);

    let addr = sim
        .listen(Sock(0), "192.0.2.37:9307".parse().unwrap())
        .unwrap();
    sim.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    sim.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();

    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(3),
            FileSlot(0),
            BufId(4),
            "/t3_07.dat",
            OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x11; 512]);
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(4),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 512,
                file_off: 0
            },
            1000
        )
        .unwrap()
        .result,
        512
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(5),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true
            },
            1000
        )
        .unwrap()
        .result,
        0
    );

    sim.submit(
        Token(10),
        Op::Recv {
            sock: Sock(2),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x22; 512]);
    sim.submit(
        Token(11),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: BufId(0),
            off: 0,
            len: 512,
            file_off: 512,
        },
    )
    .unwrap();

    sim.power_loss();
    sim.tick(false, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "power_loss must deliver zero completions for in-flight ops"
    );

    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(12),
            FileSlot(0),
            BufId(4),
            "/t3_07.dat",
            OpenFlags {
                read: true,
                write: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    let op6 = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(13),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(2),
            off: 0,
            len: 512,
            file_off: 0,
        },
        1000,
    )
    .unwrap();
    assert_eq!(op6.result, -EIO);
}

#[test]
fn t3_08_chained_write_sync_short_write_and_power_loss() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 308, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(1),
            FileSlot(0),
            BufId(4),
            "/t3_08.dat",
            OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(10),
            FileSlot(1),
            BufId(4),
            "/",
            OpenFlags {
                read: true,
                directory: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(11),
            Op::FileSync {
                slot: FileSlot(1),
                datasync: false,
            },
            1000
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(12),
            Op::FileClose { slot: FileSlot(1) },
            1000
        )
        .unwrap()
        .result,
        0
    );
    sim.place_disk_fault(5, PlacedFault::ShortWrite);
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x77; 4096]);
    sim.submit_chain(&[
        (
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        (
            Token(3),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    let cs = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();
    let w = cs.iter().find(|c| c.token == Token(2)).unwrap().result;
    let s = cs.iter().find(|c| c.token == Token(3)).unwrap().result;
    assert!(
        w > 0 && w < 4096 && w % 512 == 0,
        "short write must be sector-aligned: {w}"
    );
    assert_eq!(s, -ECANCELED, "sync after short write must be cancelled");

    sim.power_loss();
    assert_eq!(
        open_file(
            &mut sim,
            &mut out,
            Token(4),
            FileSlot(0),
            BufId(4),
            "/t3_08.dat",
            OpenFlags {
                read: true,
                ..Default::default()
            }
        )
        .unwrap(),
        0
    );
    let r = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(5),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(1),
            off: 0,
            len: 4096,
            file_off: 0,
        },
        1000,
    )
    .unwrap();
    assert!(r.result >= 0 && r.result <= w);
}
