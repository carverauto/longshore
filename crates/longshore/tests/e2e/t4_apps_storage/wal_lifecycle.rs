// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-01`: Simulated storage engine WAL lifecycle under `alloc_guard`.
//!
//! Exercises `Limits::BASE`, aligned buffer pool, `FileSlot`, `OpenFlags`,
//! `submit_chain([FileWrite, FileSync])`, segment rotation (`FileRename` +
//! dir `FileSync`), `DiskFaults`, multiple `power_loss()` events across seeds,
//! fixed-name `FileStatus` probing, and CRC-64/NVME WAL recovery.

use super::super::harness::{submit_and_wait_one, write_nul_path};
use super::wal_stack::StackWal;
use longshore::io::alloc_guard::sealed;
use longshore::io::{Completions, FileSlot, Limits, Op, OpenFlags, Token};
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};

const BLOCK_SIZE: u32 = 4096;

fn wal_limits() -> Limits {
    Limits {
        sockets_max: 4,
        buffers: 4,
        buffer_size: BLOCK_SIZE,
        small_buffers: 4,
        small_buffer_size: 256,
        aligned_buffers: 4,
        aligned_buffer_size: BLOCK_SIZE,
        block_size: BLOCK_SIZE,
        files_max: 4,
        ops_max: 16,
        ..Limits::BASE
    }
}

fn wal_disk_config() -> DiskConfig {
    DiskConfig {
        max_files: 8,
        max_dirs: 16,
        max_bytes_per_file: (BLOCK_SIZE as usize) * 32,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: (BLOCK_SIZE as usize) * 16,
        read_error_sectors: 16,
        sector_size: 512,
    }
}

fn build_wal_sim(seed: u64, disk_faults: DiskFaults) -> SimIo {
    let mut sim = SimIo::new(wal_limits(), seed, Faults::default());
    sim.setup_disk(wal_disk_config());
    let mut out = Completions::new(8);
    let path_buf = wal_limits().small(0);
    let wal_len = write_nul_path(&mut sim, path_buf, 0, "/wal");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(1),
            Op::FileMkdir {
                path_buf,
                path_off: 0,
                path_len: wal_len,
            },
            1_000,
        )
        .unwrap()
        .result,
        0
    );
    let root_len = write_nul_path(&mut sim, path_buf, 0, "/");
    assert_eq!(
        submit_and_wait_one(
            &mut sim,
            &mut out,
            Token(2),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf,
                path_off: 0,
                path_len: root_len,
                flags: OpenFlags {
                    read: true,
                    directory: true,
                    ..Default::default()
                },
            },
            1_000,
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
                slot: FileSlot(0),
                datasync: false,
            },
            1_000,
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
            Op::FileClose { slot: FileSlot(0) },
            1_000,
        )
        .unwrap()
        .result,
        0
    );
    sim.set_disk_faults(disk_faults);
    sim
}

fn fill_payload(buf: &mut [u8], seed: u64, seq: u64) -> usize {
    let len = 64 + ((seq as usize * 37) % 512);
    for (i, b) in buf[..len].iter_mut().enumerate() {
        *b = ((seed ^ seq.wrapping_mul(0x9e37_79b9)) >> ((i % 8) * 8)) as u8 ^ (i as u8);
    }
    len
}

fn run_wal_lifecycle_for_seed(seed: u64) -> (u64, u32, u64) {
    let faults = DiskFaults {
        short_write: 100_000,
        torn_reported: 60_000,
        lost_silent: 60_000,
        misdirected_write: 50_000,
        bit_flip: 40_000,
        sync_fail: 50_000,
        ..DiskFaults::default()
    };
    let mut sim = build_wal_sim(seed, faults);
    let mut out = Completions::new(16);
    let mut wal = StackWal::new(wal_limits(), BLOCK_SIZE);
    let mut committed_seqs = [0u64; 32];
    let mut committed_count = 0usize;
    let mut payload = [0u8; 640];
    let seg_bytes = u64::from(BLOCK_SIZE) * 16;

    let (valid_after_crash, last_seq_after_crash) = sealed(|| {
        assert!(wal.open_seg(&mut sim, &mut out, 1, true, true) >= 0);
        let seg1_dir_synced = wal.sync_dir(&mut sim, &mut out) == 0;

        for step in 0..8u64 {
            let plen = fill_payload(&mut payload, seed, step + 1);
            let seq = wal.next_seq;
            let (w, s) = wal.append_chained(&mut sim, &mut out, &payload[..plen]);
            if seg1_dir_synced && w == BLOCK_SIZE as i32 && s == 0 {
                committed_seqs[committed_count] = seq;
                committed_count += 1;
            }
            if wal.cursor >= seg_bytes {
                break;
            }
        }
        let _: i32 = wal.close_seg(&mut sim, &mut out);

        let _: i32 = wal.rename_seg(&mut sim, &mut out, 1, 2);
        assert!(wal.open_seg(&mut sim, &mut out, 1, true, true) >= 0);
        let seg2_dir_synced = wal.sync_dir(&mut sim, &mut out) == 0;
        wal.cursor = 0;

        for step in 8..12u64 {
            let plen = fill_payload(&mut payload, seed, step + 1);
            let seq = wal.next_seq;
            let (w, s) = wal.append_chained(&mut sim, &mut out, &payload[..plen]);
            if seg2_dir_synced && w == BLOCK_SIZE as i32 && s == 0 {
                committed_seqs[committed_count] = seq;
                committed_count += 1;
            }
        }

        sim.power_loss();
        wal.cursor = 0;

        let mut recovered = [0u64; 64];
        let mut rec_n = 0usize;
        let (_valid, _last, fault_stop) = wal.recover(&mut sim, &mut out, 4, &mut |seq| {
            if rec_n < recovered.len() {
                recovered[rec_n] = seq;
                rec_n += 1;
            }
        });
        let counters = sim.disk_fault_counters();
        let silent_write_fault = counters.lost_silent > 0 || counters.misdirected_write > 0;

        for &cseq in &committed_seqs[..committed_count] {
            assert!(
                recovered[..rec_n].contains(&cseq) || fault_stop || silent_write_fault,
                "seed {seed}: committed seq {cseq} vanished without fault stop"
            );
        }

        sim.set_disk_faults(DiskFaults::default());
        sim.power_loss();
        wal.cursor = 0;
        assert!(wal.open_seg(&mut sim, &mut out, 3, true, true) >= 0);
        let tail_len = fill_payload(&mut payload, seed, 99);
        let tail_seq = wal.next_seq;
        let (w, s) = wal.append_chained(&mut sim, &mut out, &payload[..tail_len]);
        assert_eq!((w, s), (BLOCK_SIZE as i32, 0));
        let _: i32 = wal.close_seg(&mut sim, &mut out);

        let mut saw_tail = false;
        let (final_valid, final_last, _) = wal.recover(&mut sim, &mut out, 4, &mut |seq| {
            if seq == tail_seq {
                saw_tail = true;
            }
        });
        assert!(
            saw_tail,
            "seed {seed}: post-crash appended record must recover"
        );
        (final_valid, final_last)
    });

    (sim.trace_hash(), valid_after_crash, last_seq_after_crash)
}

#[test]
fn t4_01_wal_lifecycle_faults_power_loss_and_deterministic_replay() {
    for seed in 0..10u64 {
        let first = run_wal_lifecycle_for_seed(seed);
        let replay = run_wal_lifecycle_for_seed(seed);
        assert_eq!(
            first, replay,
            "seed {seed}: WAL lifecycle must replay deterministically"
        );
        assert!(first.1 >= 1, "seed {seed}: expected recovered records");
    }
}
