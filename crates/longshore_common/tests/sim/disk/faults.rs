// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Disk fault rate tests: `short_write` chain abort, `misdirected_write`, and `read_error`.

use super::{
    drive, make_sim, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot, write_fill,
};
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
            FileSlot::new(0),
            "/chain.dat",
            rw_create()
        ),
        0
    );
    sim.set_disk_faults(DiskFaults::NONE.with_short_write(1_000_000));
    sim.buf(BufId::new(1))[..4096].fill(0x55);

    sealed(|| {
        sim.submit_chain(&[
            (
                Token::new(10),
                Op::FileWrite {
                    slot: FileSlot::new(0),
                    buf: BufId::new(1),
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
        ])
        .unwrap();
        let mut got = [None; 2];
        while got.iter().any(Option::is_none) {
            sim.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token().get() == 10 {
                    got[0] = Some(c.result());
                } else if c.token().get() == 11 {
                    got[1] = Some(c.result());
                }
            }
        }
        let written = got[0].unwrap();
        assert!(written > 0 && written < 4096 && written % 512 == 0);
        assert_eq!(got[1].unwrap(), -ECANCELED);
    });
    assert_eq!(sim.disk_fault_counters().short_write(), 1);
}

#[test]
fn misdirected_write_and_single_sector_noop() {
    let mut sim = make_sim(7, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            "/mis.dat",
            rw_create()
        ),
        0
    );
    sim.set_disk_faults(misdirect_always());
    assert_eq!(
        write_fill(&mut sim, &mut out, 2, FileSlot::new(0), 0, 512, 0x11),
        512
    );
    assert_eq!(sim.disk_fault_counters().misdirected_write(), 0);

    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        write_fill(&mut sim, &mut out, 3, FileSlot::new(0), 512, 1536, 0x11),
        1536
    );
    sim.set_disk_faults(misdirect_always());
    assert_eq!(
        write_fill(&mut sim, &mut out, 4, FileSlot::new(0), 0, 512, 0x22),
        512
    );
    assert_eq!(sim.disk_fault_counters().misdirected_write(), 1);

    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        read_at(&mut sim, &mut out, 5, FileSlot::new(0), 0, 2048),
        2048
    );
    let r = sim.buf(BufId::new(2));
    assert!(r[..512].iter().all(|&b| b == 0x11));
    assert_eq!(
        (1..4)
            .filter(|&s| r[s * 512..(s + 1) * 512].iter().all(|&b| b == 0x22))
            .count(),
        1
    );
}

fn misdirect_always() -> DiskFaults {
    DiskFaults::NONE.with_misdirected_write(1_000_000)
}

fn read_error_always() -> DiskFaults {
    DiskFaults::NONE.with_read_error(1_000_000)
}

#[test]
fn read_error_persists_across_power_loss_until_full_sector_write_or_punch() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let mut sim = make_sim(11, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, SLOT_0, "/bad.dat", rw_create()),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    assert_eq!(
        write_fill(&mut sim, &mut out, 20, SLOT_0, 0, 4096, 0x77),
        4096
    );
    assert_eq!(sync_slot(&mut sim, &mut out, 21, SLOT_0), 0);

    sim.set_disk_faults(read_error_always());
    assert_eq!(read_at(&mut sim, &mut out, 22, SLOT_0, 0, 512), -EIO);
    sim.set_disk_faults(DiskFaults::default());

    // A sub-sector write does not heal the bad sector.
    assert_eq!(
        write_fill(&mut sim, &mut out, 23, SLOT_0, 0, 256, 0x88),
        256
    );
    assert_eq!(read_at(&mut sim, &mut out, 24, SLOT_0, 0, 512), -EIO);

    sim.power_loss();
    assert_eq!(
        open_path(&mut sim, &mut out, 30, SLOT_0, "/bad.dat", rw_open()),
        0
    );
    assert_eq!(read_at(&mut sim, &mut out, 31, SLOT_0, 0, 512), -EIO);

    // A full-sector write heals it.
    assert_eq!(
        write_fill(&mut sim, &mut out, 32, SLOT_0, 0, 512, 0x99),
        512
    );
    assert_eq!(read_at(&mut sim, &mut out, 33, SLOT_0, 0, 512), 512);
    assert!(sim.buf(BufId::new(2))[..512].iter().all(|&b| b == 0x99));

    // A punch heals a fresh one.
    sim.set_disk_faults(read_error_always());
    assert_eq!(read_at(&mut sim, &mut out, 34, SLOT_0, 0, 4096), -EIO);
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            35,
            Op::FilePunch {
                slot: SLOT_0,
                file_off: 0,
                len: 4096,
            },
        ),
        0
    );
    assert_eq!(read_at(&mut sim, &mut out, 36, SLOT_0, 0, 4096), 4096);
    assert_eq!(sim.disk_fault_counters().read_error(), 2);
}
