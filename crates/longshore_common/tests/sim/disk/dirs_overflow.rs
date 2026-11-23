// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Directory log overflow and cross-directory rename of non-empty directory tests.

use super::super::{
    base_limits, dir_open, drive, open_path, read_at, rw_create, rw_open, sync_dir_path, sync_slot,
    write_fill,
};
use super::{fail_dir_sync, mkdir_path, rename_path, unlink_path};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::posix::errno::linux::ENOENT;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Op};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[test]
fn dir_log_overflow_and_cross_dir_nonempty_rename_under_sync_fail() {
    let cfg = DiskConfig {
        max_files: 8,
        max_dirs: 4,
        max_bytes_per_file: 16 * 1024,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: 16 * 1024,
        read_error_sectors: 8,
        sector_size: 512,
    };
    let mut sim = SimIo::new(base_limits(), 19, Faults::default());
    sim.setup_disk(cfg);
    let mut out = Completions::new(16);
    sealed(|| {
        assert_eq!(
            open_path(&mut sim, &mut out, 1, FileSlot(0), "/keep", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 2, FileSlot(0), 0, 16, 0x21),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 3, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 4, Op::FileClose { slot: FileSlot(0) }),
            0
        );

        assert_eq!(
            open_path(&mut sim, &mut out, 5, FileSlot(0), "/tmp_u", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 6, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        unlink_path(&mut sim, &mut out, 7, "/tmp_u");

        assert_eq!(
            open_path(&mut sim, &mut out, 8, FileSlot(0), "/m0", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 9, FileSlot(0), 0, 16, 0x33),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 10, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 11, Op::FileClose { slot: FileSlot(0) }),
            0
        );

        for i in 0..70u64 {
            let (a, b) = if i % 2 == 0 {
                ("/m0", "/m1")
            } else {
                ("/m1", "/m0")
            };
            rename_path(&mut sim, &mut out, 100 + i, a, b);
        }
        rename_path(&mut sim, &mut out, 200, "/m0", "/moved_final");
        assert_eq!(
            open_path(&mut sim, &mut out, 201, FileSlot(0), "/target", rw_create()),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 202, FileSlot(0), 0, 16, 0x44),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 203, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 204, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 205, FileSlot(0), "/ephem", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 206, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        unlink_path(&mut sim, &mut out, 207, "/ephem");

        sync_dir_path(&mut sim, &mut out, 210, "/");
        sim.power_loss();

        assert_eq!(
            open_path(&mut sim, &mut out, 220, FileSlot(0), "/keep", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 221, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0x21));
        assert_eq!(
            drive(&mut sim, &mut out, 222, Op::FileClose { slot: FileSlot(0) }),
            0
        );

        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                223,
                FileSlot(0),
                "/moved_final",
                rw_open()
            ),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 224, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0x33));
        assert_eq!(
            drive(&mut sim, &mut out, 225, Op::FileClose { slot: FileSlot(0) }),
            0
        );

        assert_eq!(
            open_path(&mut sim, &mut out, 226, FileSlot(0), "/target", rw_open()),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 227, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0x44));
        assert_eq!(
            open_path(&mut sim, &mut out, 228, FileSlot(1), "/tmp_u", rw_open()),
            -ENOENT
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 229, FileSlot(1), "/ephem", rw_open()),
            -ENOENT
        );
    });

    sim = SimIo::new(base_limits(), 29, Faults::default());
    sim.setup_disk(cfg);
    sealed(|| {
        mkdir_path(&mut sim, &mut out, 1, "/d1");
        mkdir_path(&mut sim, &mut out, 2, "/d2");
        sync_dir_path(&mut sim, &mut out, 10, "/");
        mkdir_path(&mut sim, &mut out, 20, "/d1/sub");
        sync_dir_path(&mut sim, &mut out, 30, "/d1");

        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                40,
                FileSlot(0),
                "/d1/sub/keep",
                rw_create()
            ),
            0
        );
        assert_eq!(
            write_fill(&mut sim, &mut out, 41, FileSlot(0), 0, 16, 0x55),
            16
        );
        assert_eq!(sync_slot(&mut sim, &mut out, 42, FileSlot(0)), 0);
        assert_eq!(
            drive(&mut sim, &mut out, 43, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        sync_dir_path(&mut sim, &mut out, 50, "/d1/sub");

        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                60,
                FileSlot(0),
                "/d1/sub/lost",
                rw_create()
            ),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 61, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        fail_dir_sync(&mut sim, &mut out, 70, "/d1/sub");

        assert_eq!(
            open_path(&mut sim, &mut out, 80, FileSlot(0), "/d1/p0", rw_create()),
            0
        );
        assert_eq!(
            drive(&mut sim, &mut out, 81, Op::FileClose { slot: FileSlot(0) }),
            0
        );
        for i in 0..70u64 {
            let (a, b) = if i % 2 == 0 {
                ("/d1/p0", "/d1/p1")
            } else {
                ("/d1/p1", "/d1/p0")
            };
            rename_path(&mut sim, &mut out, 100 + i, a, b);
        }

        rename_path(&mut sim, &mut out, 200, "/d1/sub", "/d2/sub");
        sync_dir_path(&mut sim, &mut out, 210, "/d1");
        sync_dir_path(&mut sim, &mut out, 220, "/d2");
        sim.power_loss();

        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                230,
                FileSlot(0),
                "/d2/sub/keep",
                rw_open()
            ),
            0
        );
        assert_eq!(read_at(&mut sim, &mut out, 231, FileSlot(0), 0, 16), 16);
        assert!(sim.buf(BufId(2))[..16].iter().all(|&b| b == 0x55));
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                232,
                FileSlot(1),
                "/d2/sub/lost",
                rw_open()
            ),
            -ENOENT
        );
        assert_eq!(
            open_path(&mut sim, &mut out, 233, FileSlot(1), "/d1/sub", dir_open()),
            -ENOENT
        );
    });
}
