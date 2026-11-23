// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulated disk tests: write-log eviction, directories, `power_loss`, and all
//! 11 [`DiskFaults`] / [`PlacedFault`] behaviors under `alloc_guard`.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{
    file_size, BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token,
};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[global_allocator]
static ALLOC: Guard = Guard;

#[path = "disk/adversarial.rs"]
mod adversarial;
#[path = "disk/adversarial_faults.rs"]
mod adversarial_faults;
#[path = "disk/dirs.rs"]
mod dirs;
#[path = "disk/faults.rs"]
mod faults;
#[path = "disk/faults_placed.rs"]
mod faults_placed;
#[path = "disk/faults_sync.rs"]
mod faults_sync;
#[path = "disk/power_loss.rs"]
mod power_loss;
#[path = "disk/power_loss_settle.rs"]
mod power_loss_settle;

pub(crate) fn base_limits() -> Limits {
    Limits {
        sockets_max: 6,
        buffers: 4,
        buffer_size: 32 * 1024,
        small_buffers: 2,
        small_buffer_size: 128,
        files_max: 4,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 16,
        ..Limits::BASE
    }
}

pub(crate) fn make_sim(seed: u64, sector_size: u32, log_entries: u32, log_bytes: usize) -> SimIo {
    let mut sim = SimIo::new(base_limits(), seed, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 16,
        max_dirs: 8,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: log_entries,
        write_log_bytes: log_bytes,
        read_error_sectors: 16,
        sector_size,
    });
    sim
}

pub(crate) fn put_path(sim: &mut SimIo, buf: BufId, path: &str) -> u32 {
    let b = path.as_bytes();
    let dst = sim.buf(buf);
    dst[..b.len()].copy_from_slice(b);
    dst[b.len()] = 0;
    (b.len() + 1) as u32
}

#[cfg(test)]
pub(crate) fn drive(sim: &mut SimIo, out: &mut Completions, tok: u64, op: Op) -> i32 {
    sim.submit(Token(tok), op).unwrap();
    for _ in 0..10_000 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(tok) {
                return c.result;
            }
        }
    }
    panic!("timed out on token {tok}");
}

pub(crate) fn rw_create() -> OpenFlags {
    OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    }
}

pub(crate) fn rw_open() -> OpenFlags {
    OpenFlags {
        read: true,
        write: true,
        ..Default::default()
    }
}

pub(crate) fn dir_open() -> OpenFlags {
    OpenFlags {
        read: true,
        directory: true,
        ..Default::default()
    }
}

pub(crate) fn open_path(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    let len = put_path(sim, BufId(0), path);
    drive(
        sim,
        out,
        tok,
        Op::FileOpen {
            slot,
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
            flags,
        },
    )
}

pub(crate) fn sync_dir_path(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) {
    assert_eq!(open_path(sim, out, tok, FileSlot(3), path, dir_open()), 0);
    assert_eq!(
        drive(
            sim,
            out,
            tok + 1,
            Op::FileSync {
                slot: FileSlot(3),
                datasync: false,
            },
        ),
        0
    );
    assert_eq!(
        drive(sim, out, tok + 2, Op::FileClose { slot: FileSlot(3) }),
        0
    );
}

pub(crate) fn write_fill(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    file_off: u64,
    len: u32,
    byte: u8,
) -> i32 {
    sim.buf(BufId(1))[..len as usize].fill(byte);
    drive(
        sim,
        out,
        tok,
        Op::FileWrite {
            slot,
            buf: BufId(1),
            off: 0,
            len,
            file_off,
        },
    )
}

pub(crate) fn read_at(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    file_off: u64,
    len: u32,
) -> i32 {
    drive(
        sim,
        out,
        tok,
        Op::FileRead {
            slot,
            buf: BufId(2),
            off: 0,
            len,
            file_off,
        },
    )
}

pub(crate) fn sync_slot(sim: &mut SimIo, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    drive(
        sim,
        out,
        tok,
        Op::FileSync {
            slot,
            datasync: false,
        },
    )
}

pub(crate) fn stat_size(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> u64 {
    let len = put_path(sim, BufId(0), path);
    assert_eq!(
        drive(
            sim,
            out,
            tok,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: len,
                out_buf: BufId(2),
                out_off: 0,
            },
        ),
        0
    );
    file_size(sim.buf(BufId(2)))
}

#[test]
#[should_panic(expected = "sector_size must be 512 or 4096")]
fn invalid_sector_size_panics_at_setup() {
    let mut sim = SimIo::new(base_limits(), 1, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 2,
        max_bytes_per_file: 4096,
        max_name_len: 64,
        sector_size: 1024,
        ..DiskConfig::BASE
    });
}

#[test]
fn write_log_overflow_evicts_oldest_to_durable_and_zero_allocs() {
    // Configure capacity for 2 entries / 1024 bytes, then issue 4 writes without FileSync.
    // The oldest 2 writes MUST be evicted to durable and survive power_loss across every seed.
    for seed in 0..8u64 {
        let mut sim = make_sim(seed, 512, 2, 1024);
        let mut out = Completions::new(16);
        assert_eq!(
            open_path(
                &mut sim,
                &mut out,
                1,
                FileSlot(0),
                "/evict.dat",
                rw_create()
            ),
            0
        );
        sync_dir_path(&mut sim, &mut out, 10, "/");

        sealed(|| {
            for idx in 0..4u64 {
                sim.buf(BufId(1))[..512].fill((0x10 + idx) as u8);
                assert_eq!(
                    drive(
                        &mut sim,
                        &mut out,
                        20 + idx,
                        Op::FileWrite {
                            slot: FileSlot(0),
                            buf: BufId(1),
                            off: 0,
                            len: 512,
                            file_off: idx * 512,
                        },
                    ),
                    512
                );
            }
            sim.power_loss();
            assert_eq!(
                open_path(&mut sim, &mut out, 30, FileSlot(0), "/evict.dat", rw_open()),
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
                        len: 1024,
                        file_off: 0,
                    },
                ),
                1024
            );
            let r = sim.buf(BufId(2));
            assert!(r[..512].iter().all(|&b| b == 0x10));
            assert!(r[512..1024].iter().all(|&b| b == 0x11));
        });
    }
}
