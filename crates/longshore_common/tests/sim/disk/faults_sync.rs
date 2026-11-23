// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Disk `sync_fail` (`AS_EIO`) poisoned overlay regression tests.

use super::{
    drive, make_sim, open_path, read_at, rw_create, rw_open, stat_size, sync_dir_path, sync_slot,
    write_fill,
};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags};
use longshore_common::sim::{DiskFaults, SimIo};

fn fail_sync(
    sim: &mut longshore_common::sim::SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
) {
    sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
    assert_eq!(sync_slot(sim, out, tok, slot), -EIO);
    sim.set_disk_faults(DiskFaults::default());
}

#[test]
fn sync_fail_as_eio_poisoned_writes_never_become_durable_unless_overwritten() {
    let mut sim = make_sim(23, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            "/eio.dat",
            rw_create()
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    assert_eq!(
        write_fill(&mut sim, &mut out, 20, FileSlot::new(0), 0, 1536, 0x11),
        1536
    );
    assert_eq!(sync_slot(&mut sim, &mut out, 21, FileSlot::new(0)), 0);

    assert_eq!(
        write_fill(&mut sim, &mut out, 22, FileSlot::new(0), 0, 1536, 0x22),
        1536
    );
    fail_sync(&mut sim, &mut out, 23, FileSlot::new(0));

    assert_eq!(
        read_at(&mut sim, &mut out, 24, FileSlot::new(0), 0, 1536),
        1536
    );
    assert!(sim.buf(BufId::new(2))[..1536].iter().all(|&b| b == 0x22));

    assert_eq!(
        write_fill(&mut sim, &mut out, 25, FileSlot::new(0), 512, 512, 0x33),
        512
    );
    assert_eq!(sync_slot(&mut sim, &mut out, 26, FileSlot::new(0)), 0);

    assert_eq!(
        read_at(&mut sim, &mut out, 27, FileSlot::new(0), 0, 1536),
        1536
    );
    let live = sim.buf(BufId::new(2));
    assert!(live[..512].iter().all(|&b| b == 0x22));
    assert!(live[512..1024].iter().all(|&b| b == 0x33));
    assert!(live[1024..1536].iter().all(|&b| b == 0x22));

    sim.power_loss();
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            30,
            FileSlot::new(0),
            "/eio.dat",
            rw_open()
        ),
        0
    );
    assert_eq!(
        read_at(&mut sim, &mut out, 31, FileSlot::new(0), 0, 1536),
        1536
    );
    let dur = sim.buf(BufId::new(2));
    assert!(dur[..512].iter().all(|&b| b == 0x11));
    assert!(dur[512..1024].iter().all(|&b| b == 0x33));
    assert!(dur[1024..1536].iter().all(|&b| b == 0x11));
}

#[test]
fn sync_fail_poisoned_write_followed_by_overwrite_and_disjoint_clean_write() {
    let mut sim = make_sim(5, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            "/s1.dat",
            rw_create()
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sealed(|| {
        assert_eq!(
            write_fill(&mut sim, &mut out, 20, FileSlot::new(0), 0, 1024, 0x11),
            1024
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 21, FileSlot::new(0)), 0);

        assert_eq!(
            write_fill(&mut sim, &mut out, 22, FileSlot::new(0), 0, 512, 0x22),
            512
        );
        sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
        assert_eq!(sync_slot(&mut sim, &mut out, 23, FileSlot::new(0)), -EIO);
        sim.set_disk_faults(DiskFaults::default());

        assert_eq!(
            write_fill(&mut sim, &mut out, 24, FileSlot::new(0), 0, 512, 0x33),
            512
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 25, FileSlot::new(0), 512, 512, 0x44),
            512
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 26, FileSlot::new(0)), 0);

        sim.power_loss();
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                30,
                FileSlot::new(0),
                "/s1.dat",
                rw_open()
            ),
            0
        );
        assert_eq!(
            read_at(&mut sim, &mut out, 31, FileSlot::new(0), 0, 1024),
            1024
        );
        let dur = sim.buf(BufId::new(2));
        assert!(dur[..512].iter().all(|&b| b == 0x33));
        assert!(dur[512..1024].iter().all(|&b| b == 0x44));
    });
}

#[test]
fn sync_fail_poisoned_punch_followed_by_partial_write_and_sync() {
    let mut sim = make_sim(7, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot::new(0),
            "/pun.dat",
            rw_create()
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sealed(|| {
        assert_eq!(
            write_fill(&mut sim, &mut out, 20, FileSlot::new(0), 0, 4096, 0xAA),
            4096
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 21, FileSlot::new(0)), 0);

        assert_eq!(
            drive(
                &mut sim,
                &mut out,
                22,
                Op::FilePunch {
                    slot: FileSlot::new(0),
                    file_off: 0,
                    len: 4096,
                },
            ),
            0
        );
        sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
        assert_eq!(sync_slot(&mut sim, &mut out, 23, FileSlot::new(0)), -EIO);
        sim.set_disk_faults(DiskFaults::default());

        assert_eq!(
            write_fill(&mut sim, &mut out, 24, FileSlot::new(0), 512, 512, 0xBB),
            512
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 25, FileSlot::new(0)), 0);

        assert_eq!(
            read_at(&mut sim, &mut out, 26, FileSlot::new(0), 0, 4096),
            4096
        );
        let live = sim.buf(BufId::new(2));
        assert!(live[..512].iter().all(|&b| b == 0));
        assert!(live[512..1024].iter().all(|&b| b == 0xBB));
        assert!(live[1024..4096].iter().all(|&b| b == 0));

        sim.power_loss();
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                30,
                FileSlot::new(0),
                "/pun.dat",
                rw_open()
            ),
            0
        );
        assert_eq!(
            read_at(&mut sim, &mut out, 31, FileSlot::new(0), 0, 4096),
            4096
        );
        let dur = sim.buf(BufId::new(2));
        assert!(dur[..512].iter().all(|&b| b == 0xAA));
        assert!(dur[512..1024].iter().all(|&b| b == 0xBB));
        assert!(dur[1024..4096].iter().all(|&b| b == 0xAA));
    });
}

#[test]
fn sync_fail_poisoned_truncate_followed_by_zero_and_nonzero_writes() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let mut sim = make_sim(9, 512, 16, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, SLOT_0, "/tr0.dat", rw_create()),
        0
    );
    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            2,
            FileSlot::new(1),
            "/tr1.dat",
            rw_create()
        ),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    let trunc_flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_empty(true);
    sealed(|| {
        for (idx, (slot, path)) in [(SLOT_0, "/tr0.dat"), (FileSlot::new(1), "/tr1.dat")]
            .into_iter()
            .enumerate()
        {
            let b = 20 + idx as u64 * 10;
            assert_eq!(write_fill(&mut sim, &mut out, b, slot, 0, 4096, 0x11), 4096);
            assert_eq!(sync_slot(&mut sim, &mut out, b + 1, slot), 0);
            assert_eq!(drive(&mut sim, &mut out, b + 2, Op::FileClose { slot }), 0);
            assert_eq!(
                open_path(&mut sim, &mut out, b + 3, slot, path, trunc_flags),
                0
            );
            fail_sync(&mut sim, &mut out, b + 4, slot);
        }

        assert_eq!(
            write_fill(&mut sim, &mut out, 50, SLOT_0, 0, 512, 0x22),
            512
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 51, SLOT_0), 0);
        assert_eq!(stat_size(&mut sim, &mut out, 52, "/tr0.dat"), 512);
        assert_eq!(read_at(&mut sim, &mut out, 53, SLOT_0, 0, 4096), 512);
        assert!(sim.buf(BufId::new(2))[..512].iter().all(|&b| b == 0x22));

        assert_eq!(
            write_fill(&mut sim, &mut out, 60, FileSlot::new(1), 1024, 512, 0x33),
            512
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 61, FileSlot::new(1)), 0);
        assert_eq!(stat_size(&mut sim, &mut out, 62, "/tr1.dat"), 1536);
        assert_eq!(
            read_at(&mut sim, &mut out, 63, FileSlot::new(1), 0, 4096),
            1536
        );
        let live1 = sim.buf(BufId::new(2));
        assert!(live1[..1024].iter().all(|&b| b == 0));
        assert!(live1[1024..1536].iter().all(|&b| b == 0x33));

        sim.power_loss();
        assert_poisoned_truncates_rolled_back(&mut sim, &mut out);
    });
}

/// After power loss both files are 4096 bytes again: the poisoned truncate never landed,
/// and only the later synced writes sit on top of the old `0x11` data.
fn assert_poisoned_truncates_rolled_back(sim: &mut SimIo, out: &mut Completions) {
    assert_eq!(stat_size(sim, out, 70, "/tr0.dat"), 4096);
    assert_eq!(
        open_path(sim, out, 71, FileSlot::new(0), "/tr0.dat", rw_open()),
        0
    );
    assert_eq!(read_at(sim, out, 72, FileSlot::new(0), 0, 4096), 4096);
    let dur0 = sim.buf(BufId::new(2));
    assert!(dur0[..512].iter().all(|&b| b == 0x22));
    assert!(dur0[512..4096].iter().all(|&b| b == 0x11));

    assert_eq!(stat_size(sim, out, 80, "/tr1.dat"), 4096);
    assert_eq!(
        open_path(sim, out, 81, FileSlot::new(1), "/tr1.dat", rw_open()),
        0
    );
    assert_eq!(read_at(sim, out, 82, FileSlot::new(1), 0, 4096), 4096);
    let dur1 = sim.buf(BufId::new(2));
    assert!(dur1[..1024].iter().all(|&b| b == 0x11));
    assert!(dur1[1024..1536].iter().all(|&b| b == 0x33));
    assert!(dur1[1536..4096].iter().all(|&b| b == 0x11));
}

#[test]
fn sync_fail_many_non_adjacent_clean_writes_never_truncate_poisoned_tail() {
    const SLOT_0: FileSlot = FileSlot::new(0);
    let mut sim = make_sim(13, 512, 2, 16 * 1024);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, SLOT_0, "/frag.dat", rw_create()),
        0
    );
    sync_dir_path(&mut sim, &mut out, 10, "/");
    sealed(|| {
        assert_eq!(
            write_fill(&mut sim, &mut out, 20, SLOT_0, 0, 4096, 0x11),
            4096
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 21, SLOT_0), 0);

        assert_eq!(
            write_fill(&mut sim, &mut out, 22, SLOT_0, 0, 4096, 0x22),
            4096
        );
        sim.set_disk_faults(DiskFaults::NONE.with_sync_fail(1_000_000));
        assert_eq!(sync_slot(&mut sim, &mut out, 23, SLOT_0), -EIO);
        sim.set_disk_faults(DiskFaults::default());

        let patches = [100u64, 500, 900, 1300, 1700, 2100];
        for (idx, &off) in patches.iter().enumerate() {
            let b = 30 + idx as u64 * 2;
            assert_eq!(
                write_fill(&mut sim, &mut out, b, SLOT_0, off, 100, 0x33),
                100
            );
            assert_eq!(sync_slot(&mut sim, &mut out, b + 1, SLOT_0), 0);
        }

        assert_eq!(read_at(&mut sim, &mut out, 60, SLOT_0, 0, 4096), 4096);
        let live = sim.buf(BufId::new(2));
        for (i, &byte) in live[..4096].iter().enumerate() {
            let in_patch = patches
                .iter()
                .any(|&p| (p as usize..p as usize + 100).contains(&i));
            assert_eq!(
                byte,
                if in_patch { 0x33 } else { 0x22 },
                "live mismatch at {i}"
            );
        }

        sim.power_loss();
        assert_eq!(
            open_path(&mut sim, &mut out, 70, SLOT_0, "/frag.dat", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 71, SLOT_0, 0, 4096), 4096);
        let dur = sim.buf(BufId::new(2));
        for (i, &byte) in dur[..4096].iter().enumerate() {
            let in_patch = patches
                .iter()
                .any(|&p| (p as usize..p as usize + 100).contains(&i));
            assert_eq!(
                byte,
                if in_patch { 0x33 } else { 0x11 },
                "durable mismatch at {i}"
            );
        }
    });
}
