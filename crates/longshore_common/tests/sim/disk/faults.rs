// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Disk fault rate tests: `short_write` chain abort, `misdirected_write`, and `read_error`.

use super::{drive, make_sim, open_path, rw_create, rw_open, sync_dir_path};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::{ECANCELED, EIO};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op, Token};
use longshore_common::sim::DiskFaults;

#[test]
fn short_write_in_chain_cancels_sync_and_updates_counter() {
    let mut sim = make_sim(3, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot(0),
            "/chain.dat",
            rw_create()
        ),
        0
    );
    sim.set_disk_faults(DiskFaults {
        short_write: 1_000_000,
        ..Default::default()
    });
    sim.buf(BufId(1))[..4096].fill(0x55);

    sealed(|| {
        sim.submit_chain(&[
            (
                Token(10),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: 0,
                    len: 4096,
                    file_off: 0,
                },
            ),
            (
                Token(11),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let mut got = [None; 2];
        while got.iter().any(Option::is_none) {
            sim.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == Token(10) {
                    got[0] = Some(c.result);
                } else if c.token == Token(11) {
                    got[1] = Some(c.result);
                }
            }
        }
        let written = got[0].unwrap();
        assert!(written > 0 && written < 4096 && written % 512 == 0);
        assert_eq!(got[1].unwrap(), -ECANCELED);
    });
    assert_eq!(sim.disk_fault_counters().short_write, 1);
}

#[test]
fn misdirected_write_and_single_sector_noop() {
    let mut sim = make_sim(7, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, FileSlot(0), "/mis.dat", rw_create()),
        0
    );
    sim.set_disk_faults(DiskFaults {
        misdirected_write: 1_000_000,
        ..Default::default()
    });
    sim.buf(BufId(1))[..512].fill(0x11);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            2,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        512
    );
    assert_eq!(sim.disk_fault_counters().misdirected_write, 0);

    sim.set_disk_faults(DiskFaults::default());
    sim.buf(BufId(1))[..1536].fill(0x11);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            3,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 1536,
                file_off: 512,
            },
        ),
        1536
    );
    sim.set_disk_faults(DiskFaults {
        misdirected_write: 1_000_000,
        ..Default::default()
    });
    sim.buf(BufId(1))[..512].fill(0x22);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            4,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        512
    );
    assert_eq!(sim.disk_fault_counters().misdirected_write, 1);

    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            5,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 2048,
                file_off: 0,
            },
        ),
        2048
    );
    let r = sim.buf(BufId(2));
    assert!(r[..512].iter().all(|&b| b == 0x11));
    assert_eq!(
        (1..4)
            .filter(|&s| r[s * 512..(s + 1) * 512].iter().all(|&b| b == 0x22))
            .count(),
        1
    );
}

#[test]
fn read_error_persists_across_power_loss_until_full_sector_write_or_punch() {
    let mut sim = make_sim(11, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, FileSlot(0), "/bad.dat", rw_create()),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sim.buf(BufId(1))[..4096].fill(0x77);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            20,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        4096
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            21,
            Op::FileSync {
                slot: FileSlot(0),
                datasync: false,
            },
        ),
        0
    );

    sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            22,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        -EIO
    );
    sim.set_disk_faults(DiskFaults::default());

    sim.buf(BufId(1))[..256].fill(0x88);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            23,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 256,
                file_off: 0,
            },
        ),
        256
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            24,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        -EIO
    );

    sim.power_loss();
    assert_eq!(
        open_path(&mut sim, &mut out, 30, FileSlot(0), "/bad.dat", rw_open()),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            31,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        -EIO
    );

    sim.buf(BufId(1))[..512].fill(0x99);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            32,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        512
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            33,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 512,
                file_off: 0,
            },
        ),
        512
    );
    assert!(sim.buf(BufId(2))[..512].iter().all(|&b| b == 0x99));

    sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..Default::default()
    });
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            34,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        -EIO
    );
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            35,
            Op::FilePunch {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
            },
        ),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            36,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        4096
    );
    assert_eq!(sim.disk_fault_counters().read_error, 2);
}
