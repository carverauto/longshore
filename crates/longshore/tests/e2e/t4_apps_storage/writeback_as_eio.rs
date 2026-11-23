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

/// The writeback scenario's simulator, completions, and buffers.
struct Rig {
    sim: SimIo,
    out: Completions,
    path_buf: BufId,
    w_buf: BufId,
    r_buf: BufId,
}

impl Rig {
    /// Submits `op` and waits up to 1000 ticks for its result.
    fn wait(&mut self, token: Token, op: Op) -> i32 {
        submit_and_wait_one(&mut self.sim, &mut self.out, token, op, 1_000)
            .unwrap()
            .result
    }

    /// Opens `/wb_eio.dat` read-write in `FileSlot(0)`, creating it if asked.
    fn open_file(&mut self, token: Token, create: bool) -> i32 {
        let plen = write_nul_path(&mut self.sim, self.path_buf, 0, "/wb_eio.dat");
        let open = Op::FileOpen {
            slot: FileSlot(0),
            path_buf: self.path_buf,
            path_off: 0,
            path_len: plen,
            flags: OpenFlags {
                read: true,
                write: true,
                create,
                ..Default::default()
            },
        };
        self.wait(token, open)
    }

    /// Opens, syncs, and closes `/` so the file name is durable after a crash.
    fn sync_root_dir(&mut self) {
        let dlen = write_nul_path(&mut self.sim, self.path_buf, 0, "/");
        let open_dir = Op::FileOpen {
            slot: FileSlot(1),
            path_buf: self.path_buf,
            path_off: 0,
            path_len: dlen,
            flags: OpenFlags {
                read: true,
                directory: true,
                ..Default::default()
            },
        };
        assert_eq!(self.wait(Token(2), open_dir), 0);
        let sync_dir = Op::FileSync {
            slot: FileSlot(1),
            datasync: false,
        };
        assert_eq!(self.wait(Token(3), sync_dir), 0);
        assert_eq!(self.wait(Token(4), Op::FileClose { slot: FileSlot(1) }), 0);
    }

    /// Writes sector `sec` filled with `byte`; returns the write result.
    fn write_sector(&mut self, token: Token, sec: u64, byte: u8) -> i32 {
        write_pool_bytes(&mut self.sim, self.w_buf, 0, &[byte; SECTOR as usize]);
        let write = Op::FileWrite {
            slot: FileSlot(0),
            buf: self.w_buf,
            off: 0,
            len: SECTOR as u32,
            file_off: sec * SECTOR,
        };
        self.wait(token, write)
    }

    /// Reads sector `sec` into the read buffer; returns the read result.
    fn read_sector_result(&mut self, token: Token, sec: u64) -> i32 {
        let read = Op::FileRead {
            slot: FileSlot(0),
            buf: self.r_buf,
            off: 0,
            len: SECTOR as u32,
            file_off: sec * SECTOR,
        };
        self.wait(token, read)
    }

    /// Reads all of sector `sec` and returns its bytes.
    fn read_sector(&mut self, token: Token, sec: u64) -> [u8; SECTOR as usize] {
        assert_eq!(self.read_sector_result(token, sec), SECTOR as i32);
        let mut got = [0u8; SECTOR as usize];
        read_pool_into(&mut self.sim, self.r_buf, 0, &mut got);
        got
    }

    /// A full (non-data) `FileSync` of the file; returns its result.
    fn sync_file(&mut self, token: Token) -> i32 {
        let sync = Op::FileSync {
            slot: FileSlot(0),
            datasync: false,
        };
        self.wait(token, sync)
    }
}

#[test]
fn t4_11_writeback_eviction_as_eio_poison_and_crc_sector_repair() {
    let lim = storage_limits();
    let mut rig = Rig {
        sim: setup_small_writeback_sim(404),
        out: Completions::new(8),
        path_buf: lim.small(0),
        w_buf: BufId(0),
        r_buf: BufId(1),
    };

    // 1. Open and sync directory + initial file so name is durable.
    assert_eq!(rig.open_file(Token(1), true), 0);
    rig.sync_root_dir();

    // 2. Write 4 sectors without sync: log capacity is 3, so sector 0 evicts to durable!
    for sec in 0..4u64 {
        assert_eq!(
            rig.write_sector(Token(10 + sec), sec, 0xA0 + sec as u8),
            SECTOR as i32
        );
    }

    // 3. Inject placed Fail on the next FileSync (op #9): writes in log become poisoned (AS_EIO).
    // Op counts: 1=open file, 2=open dir, 3=sync dir, 4=close dir, 5..8=4 writes, 9=next FileSync.
    rig.sim.place_disk_fault(9, PlacedFault::Fail);
    assert_eq!(rig.sync_file(Token(20)), -EIO);

    // 4. Second FileSync succeeds, but Linux AS_EIO means poisoned writes are NOT durable.
    assert_eq!(rig.sync_file(Token(21)), 0);

    // Before power loss, poisoned sector 3 is still readable in page cache / overlay.
    let pre = rig.read_sector(Token(22), 3);
    assert!(pre.iter().all(|&b| b == 0xA3));

    // 5. Power loss drops poisoned writes while evicted sector 0 remains durable.
    rig.sim.power_loss();
    assert_eq!(rig.open_file(Token(30), false), 0);
    let post0 = rig.read_sector(Token(31), 0);
    assert!(
        post0.iter().all(|&b| b == 0xA0),
        "evicted sector 0 must survive"
    );

    // 6. Corrupt sector 0 medium bit, detect via CRC-64/NVME, inject read_error and repair!
    let expected_crc = crc64_nvme(&[0xA0u8; SECTOR as usize]);
    rig.sim.corrupt_medium_bit("/wb_eio.dat", 16, 3).unwrap();
    let flipped = rig.read_sector(Token(32), 0);
    assert_ne!(
        crc64_nvme(&flipped),
        expected_crc,
        "CRC must catch bit flip"
    );

    // Inject persistent read_error on sector 1 via DiskFaults, observe -EIO, then repair via full-sector write.
    rig.sim.set_disk_faults(DiskFaults {
        read_error: 1_000_000,
        ..DiskFaults::default()
    });
    assert_eq!(rig.read_sector_result(Token(33), 1), -EIO);

    rig.sim.set_disk_faults(DiskFaults::default());
    // Repair sector 0 and sector 1 with clean full-sector writes + sync.
    for (idx, byte) in [(0u64, 0xA0u8), (1u64, 0xA1u8)] {
        assert_eq!(rig.write_sector(Token(40 + idx), idx, byte), SECTOR as i32);
    }
    let repaired = rig.read_sector(Token(50), 0);
    assert_eq!(crc64_nvme(&repaired), expected_crc);
}
