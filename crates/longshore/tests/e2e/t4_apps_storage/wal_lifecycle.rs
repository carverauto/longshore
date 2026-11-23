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

/// Submits `op` and waits up to 1000 ticks for its result.
fn wait_result(sim: &mut SimIo, out: &mut Completions, token: Token, op: Op) -> i32 {
    submit_and_wait_one(sim, out, token, op, 1_000)
        .unwrap()
        .result
}

fn build_wal_sim(seed: u64, disk_faults: DiskFaults) -> SimIo {
    let mut sim = SimIo::new(wal_limits(), seed, Faults::default());
    sim.setup_disk(wal_disk_config());
    let mut out = Completions::new(8);
    let path_buf = wal_limits().small(0);
    let wal_len = write_nul_path(&mut sim, path_buf, 0, "/wal");
    let mkdir = Op::FileMkdir {
        path_buf,
        path_off: 0,
        path_len: wal_len,
    };
    assert_eq!(wait_result(&mut sim, &mut out, Token(1), mkdir), 0);
    let root_len = write_nul_path(&mut sim, path_buf, 0, "/");
    let open_root = Op::FileOpen {
        slot: FileSlot(0),
        path_buf,
        path_off: 0,
        path_len: root_len,
        flags: OpenFlags {
            read: true,
            directory: true,
            ..Default::default()
        },
    };
    assert_eq!(wait_result(&mut sim, &mut out, Token(2), open_root), 0);
    let sync_root = Op::FileSync {
        slot: FileSlot(0),
        datasync: false,
    };
    assert_eq!(wait_result(&mut sim, &mut out, Token(3), sync_root), 0);
    let close = Op::FileClose { slot: FileSlot(0) };
    assert_eq!(wait_result(&mut sim, &mut out, Token(4), close), 0);
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

/// Sequence numbers whose write and sync both succeeded after a synced directory.
struct Committed {
    seqs: [u64; 32],
    count: usize,
}

/// The per-seed WAL session: simulator, completions, WAL driver, and payload scratch.
struct Session<'a> {
    sim: &'a mut SimIo,
    out: &'a mut Completions,
    wal: &'a mut StackWal,
    payload: &'a mut [u8; 640],
    seed: u64,
}

impl Session<'_> {
    /// Appends one record per step, recording the committed ones; stops early
    /// once the cursor reaches `seg_limit`, if given.
    fn append_steps(
        &mut self,
        steps: std::ops::Range<u64>,
        dir_synced: bool,
        seg_limit: Option<u64>,
        committed: &mut Committed,
    ) {
        for step in steps {
            let plen = fill_payload(self.payload, self.seed, step + 1);
            let seq = self.wal.next_seq;
            let (w, s) = self
                .wal
                .append_chained(self.sim, self.out, &self.payload[..plen]);
            if dir_synced && w == i32::try_from(BLOCK_SIZE).unwrap() && s == 0 {
                committed.seqs[committed.count] = seq;
                committed.count += 1;
            }
            if seg_limit.is_some_and(|limit| self.wal.cursor >= limit) {
                break;
            }
        }
    }

    /// Opens segment 1 and syncs the directory; returns whether the sync held.
    fn open_seg1_synced(&mut self) -> bool {
        assert!(self.wal.open_seg(self.sim, self.out, 1, true, true) >= 0);
        self.wal.sync_dir(self.sim, self.out) == 0
    }

    /// Crashes under faults and checks every committed record recovers unless
    /// recovery stopped on a fault or a silent write fault occurred.
    fn crash_keeps_committed(&mut self, committed: &Committed) {
        self.sim.power_loss();
        self.wal.cursor = 0;

        let mut recovered = [0u64; 64];
        let mut rec_n = 0usize;
        let (_valid, _last, fault_stop) = self.wal.recover(self.sim, self.out, 4, &mut |seq| {
            if rec_n < recovered.len() {
                recovered[rec_n] = seq;
                rec_n += 1;
            }
        });
        let counters = self.sim.disk_fault_counters();
        let silent_write_fault = counters.lost_silent > 0 || counters.misdirected_write > 0;

        let seed = self.seed;
        for &cseq in &committed.seqs[..committed.count] {
            assert!(
                recovered[..rec_n].contains(&cseq) || fault_stop || silent_write_fault,
                "seed {seed}: committed seq {cseq} vanished without fault stop"
            );
        }
    }

    /// With faults off, crashes again, appends a tail record to segment 3, and
    /// checks it recovers; returns the final valid count and last sequence.
    fn clean_tail_recovers(&mut self) -> (u32, u64) {
        self.sim.set_disk_faults(DiskFaults::default());
        self.sim.power_loss();
        self.wal.cursor = 0;
        assert!(self.wal.open_seg(self.sim, self.out, 3, true, true) >= 0);
        let tail_len = fill_payload(self.payload, self.seed, 99);
        let tail_seq = self.wal.next_seq;
        let (w, s) = self
            .wal
            .append_chained(self.sim, self.out, &self.payload[..tail_len]);
        assert_eq!((w, s), (i32::try_from(BLOCK_SIZE).unwrap(), 0));
        let _: i32 = self.wal.close_seg(self.sim, self.out);

        let mut saw_tail = false;
        let (final_valid, final_last, _) = self.wal.recover(self.sim, self.out, 4, &mut |seq| {
            if seq == tail_seq {
                saw_tail = true;
            }
        });
        let seed = self.seed;
        assert!(
            saw_tail,
            "seed {seed}: post-crash appended record must recover"
        );
        (final_valid, final_last)
    }
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
    let mut committed = Committed {
        seqs: [0u64; 32],
        count: 0,
    };
    let mut payload = [0u8; 640];
    let seg_bytes = u64::from(BLOCK_SIZE) * 16;

    let (valid_after_crash, last_seq_after_crash) = sealed(|| {
        let mut session = Session {
            sim: &mut sim,
            out: &mut out,
            wal: &mut wal,
            payload: &mut payload,
            seed,
        };
        let seg1_dir_synced = session.open_seg1_synced();
        session.append_steps(0..8, seg1_dir_synced, Some(seg_bytes), &mut committed);
        let _: i32 = session.wal.close_seg(session.sim, session.out);

        let _: i32 = session.wal.rename_seg(session.sim, session.out, 1, 2);
        let seg2_dir_synced = session.open_seg1_synced();
        session.wal.cursor = 0;
        session.append_steps(8..12, seg2_dir_synced, None, &mut committed);

        session.crash_keeps_committed(&committed);
        session.clean_tail_recovers()
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
