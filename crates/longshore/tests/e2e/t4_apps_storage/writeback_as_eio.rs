// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-11`: Simulated writeback eviction + poisoned `FileSync` (`AS_EIO`) +
//! bit-flip CRC detection and full-sector repair.

use super::super::harness::{
    read_pool_into, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use longshore::checksum::crc64_nvme;
use longshore::io::{BufId, Completions, FileSlot, Limits, Op, OpenFlags, Token};
use longshore::sim::{DiskConfig, DiskFaults, Faults, PlacedFault, SimIo};

const EIO: i32 = 5;

const SECTOR: u64 = 512;
const BLOCK: u32 = 4096;

fn storage_limits() -> Limits {
    Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: BLOCK,
        small_buffers: 2,
        small_buffer_size: 128,
        aligned_buffers: 2,
        aligned_buffer_size: BLOCK,
        block_size: BLOCK,
        files_max: 4,
        ops_max: 8,
        ..Limits::BASE
    }
}

fn setup_small_writeback_sim(seed: u64) -> SimIo {
    let mut sim = SimIo::new(storage_limits(), seed, Faults::default());
    // Tiny write_log_entries (3) so 4th unsynced write forces oldest writeback eviction.
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 8,
        max_bytes_per_file: (BLOCK as usize) * 16,
        max_name_len: 64,
        write_log_entries: 3,
        write_log_bytes: (SECTOR as usize) * 8,
        read_error_sectors: 8,
        sector_size: SECTOR as u32,
    });
    sim
}

#[test]
fn t4_11_writeback_eviction_as_eio_poison_and_crc_sector_repair() {
    let lim = storage_limits();
    let mut sim = setup_small_writeback_sim(404);
    let mut out = Completions::new(8);
    let path_buf = lim.small(0);
    let w_buf = BufId(0);
    let r_buf = BufId(1);
    let mut plen = write_nul_path(&mut sim, path_buf, 0, "/wb_eio.dat");

    // 1. Open and sync directory + initial file so name is durable.
    let c = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf,
            path_off: 0,
            path_len: plen,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        },
        1_000,
    )
    .unwrap();
    assert_eq!(c.result, 0);

    // Sync dir so /wb_eio.dat exists durably after crash.
    let dlen = write_nul_path(&mut sim, path_buf, 0, "/");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(2),
            Op::FileOpen {
                slot: FileSlot(1),
                path_buf,
                path_off: 0,
                path_len: dlen,
                flags: OpenFlags {
                    read: true,
                    directory: true,
                    ..Default::default()
                },
            },
            1_000
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(3),
            Op::FileSync {
                slot: FileSlot(1),
                datasync: false,
            },
            1_000
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(4),
            Op::FileClose { slot: FileSlot(1) },
            1_000
        )
        .unwrap()
        .result,
        0
    );

    // 2. Write 4 sectors without sync: log capacity is 3, so sector 0 evicts to durable!
    for sec in 0..4u64 {
        let data = [0xA0 + sec as u8; SECTOR as usize];
        write_pool_bytes(&mut sim, w_buf, 0, &data);
        let res = submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(10 + sec),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: w_buf,
                off: 0,
                len: SECTOR as u32,
                file_off: sec * SECTOR,
            },
            1_000,
        )
        .unwrap()
        .result;
        assert_eq!(res, SECTOR as i32);
    }

    // 3. Inject placed Fail on the next FileSync (op #9): writes in log become poisoned (AS_EIO).
    // Op counts: 1=open file, 2=open dir, 3=sync dir, 4=close dir, 5..8=4 writes, 9=next FileSync.
    sim.place_disk_fault(9, PlacedFault::Fail);
    let failed_sync = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(20),
        Op::FileSync {
            slot: FileSlot(0),
            datasync: false,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(failed_sync.result, -EIO);

    // 4. Second FileSync succeeds, but Linux AS_EIO means poisoned writes are NOT durable.
    let ok_sync = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(21),
        Op::FileSync {
            slot: FileSlot(0),
            datasync: false,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(ok_sync.result, 0);

    // Before power loss, poisoned sector 3 is still readable in page cache / overlay.
    let mut pre = [0u8; SECTOR as usize];
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(22),
            Op::FileRead {
                slot: FileSlot(0),
                buf: r_buf,
                off: 0,
                len: SECTOR as u32,
                file_off: 3 * SECTOR,
            },
            1_000
        )
        .unwrap()
        .result,
        SECTOR as i32
    );
    read_pool_into(&mut sim, r_buf, 0, &mut pre);
    assert!(pre.iter().all(|&b| b == 0xA3));

    // 5. Power loss drops poisoned writes while evicted sector 0 remains durable.
    sim.power_loss();
    plen = write_nul_path(&mut sim, path_buf, 0, "/wb_eio.dat");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(30),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf,
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    ..Default::default()
                },
            },
            1_000
        )
        .unwrap()
        .result,
        0
    );
    let mut post0 = [0u8; SECTOR as usize];
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(31),
            Op::FileRead {
                slot: FileSlot(0),
                buf: r_buf,
                off: 0,
                len: SECTOR as u32,
                file_off: 0,
            },
            1_000
        )
        .unwrap()
        .result,
        SECTOR as i32
    );
    read_pool_into(&mut sim, r_buf, 0, &mut post0);
    assert!(
        post0.iter().all(|&b| b == 0xA0),
        "evicted sector 0 must survive"
    );

    // 6. Corrupt sector 0 medium bit, detect via CRC-64/NVME, inject read_error and repair!
    let expected_crc = crc64_nvme(&[0xA0u8; SECTOR as usize]);
    sim.corrupt_medium_bit("/wb_eio.dat", 16, 3).unwrap();
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(32),
            Op::FileRead {
                slot: FileSlot(0),
                buf: r_buf,
                off: 0,
                len: SECTOR as u32,
                file_off: 0,
            },
            1_000
        )
        .unwrap()
        .result,
        SECTOR as i32
    );
    read_pool_into(&mut sim, r_buf, 0, &mut post0);
    assert_ne!(crc64_nvme(&post0), expected_crc, "CRC must catch bit flip");

    // Inject persistent read_error on sector 1 via DiskFaults, observe -EIO, then repair via full-sector write.
    sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..DiskFaults::default()
    });
    let err_c = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(33),
        Op::FileRead {
            slot: FileSlot(0),
            buf: r_buf,
            off: 0,
            len: SECTOR as u32,
            file_off: SECTOR,
        },
        1_000,
    )
    .unwrap();
    assert_eq!(err_c.result, -EIO);

    sim.set_disk_faults(DiskFaults::default());
    // Repair sector 0 and sector 1 with clean full-sector writes + sync.
    for (idx, byte) in [(0u64, 0xA0u8), (1u64, 0xA1u8)] {
        write_pool_bytes(&mut sim, w_buf, 0, &[byte; SECTOR as usize]);
        assert_eq!(
            submit_and_wait_one(
                &mut sim,
                &mut out,
                Token(40 + idx),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: w_buf,
                    off: 0,
                    len: SECTOR as u32,
                    file_off: idx * SECTOR,
                },
                1_000
            )
            .unwrap()
            .result,
            SECTOR as i32
        );
    }
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(50),
            Op::FileRead {
                slot: FileSlot(0),
                buf: r_buf,
                off: 0,
                len: SECTOR as u32,
                file_off: 0,
            },
            1_000
        )
        .unwrap()
        .result,
        SECTOR as i32
    );
    read_pool_into(&mut sim, r_buf, 0, &mut post0);
    assert_eq!(crc64_nvme(&post0), expected_crc);
}
