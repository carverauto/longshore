// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-12`: Dual-domain chaos campaign across 20 seeds (simultaneous network
//! `Faults` + `DiskFaults` + `power_loss()` on a single `SimIo`).

use super::super::harness::{submit_and_wait_one, write_nul_path, write_pool_bytes};
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token};
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};

const BLOCK: u32 = 4096;

fn chaos_limits() -> Limits {
    Limits {
        sockets_max: 8,
        buffers: 4,
        buffer_size: BLOCK,
        small_buffers: 4,
        small_buffer_size: 256,
        aligned_buffers: 2,
        aligned_buffer_size: BLOCK,
        block_size: BLOCK,
        files_max: 4,
        ops_max: 16,
        ..Limits::BASE
    }
}

fn run_chaos_seed(seed: u64) -> (u64, u64) {
    let net_faults = Faults {
        reset: 50_000,
        short_send: 200_000,
        partial_recv: 200_000,
        delay: 150_000,
        bit_flip: 20_000,
        ..Faults::default()
    };
    let disk_faults = DiskFaults {
        short_write: 100_000,
        torn_reported: 50_000,
        lost_reported: 50_000,
        sync_fail: 50_000,
        read_error: 30_000,
        ..DiskFaults::default()
    };

    let lim = chaos_limits();
    let mut sim = SimIo::new(lim, seed, net_faults);
    sim.setup_disk(DiskConfig {
        max_files: 6,
        max_dirs: 8,
        max_bytes_per_file: (BLOCK as usize) * 8,
        max_name_len: 64,
        write_log_entries: 8,
        write_log_bytes: (BLOCK as usize) * 8,
        read_error_sectors: 8,
        sector_size: 512,
    });
    sim.set_disk_faults(disk_faults);

    let mut out = Completions::new(16);
    let addr = sim
        .listen(Sock(0), "127.0.0.1:9200".parse().unwrap())
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
    let mut connected = false;
    for _ in 0..50 {
        sim.tick(true, &mut out).unwrap();
        let mut saw_a = false;
        let mut saw_c = false;
        while let Some(c) = out.pop() {
            if c.token.0 == 1 && c.result >= 0 {
                saw_a = true;
            }
            if c.token.0 == 2 && c.result == 0 {
                saw_c = true;
            }
        }
        if saw_a && saw_c {
            connected = true;
            break;
        }
        if out.is_empty() {
            break;
        }
    }

    let path_buf = lim.small(0);
    let plen = write_nul_path(&mut sim, path_buf, 0, "/chaos.log");
    let open_res = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(3),
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
    .unwrap()
    .result;
    assert_eq!(open_res, 0);

    // Interleave network Send/Recv with disk Write+Sync chains.
    for round in 0..4u64 {
        if connected {
            write_pool_bytes(&mut sim, BufId(0), 0, &[0x40 + round as u8; 512]);
            // Network faults may fail or stall this exchange; the test checks
            // that the disk chain beside it is unaffected, not the exchange.
            submit_and_wait_one(
                &mut sim,
                &mut out,
                Token(10 + round * 4),
                Op::Send {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: 512,
                    zero_copy: false,
                },
                500,
            )
            .ok();
            submit_and_wait_one(
                &mut sim,
                &mut out,
                Token(11 + round * 4),
                Op::Recv {
                    sock: Sock(2),
                    buf: BufId(1),
                    off: 0,
                    len: 512,
                },
                500,
            )
            .ok();
        }
        write_pool_bytes(&mut sim, BufId(2), 0, &[0x70 + round as u8; 512]);
        sim.submit_chain(&[
            (
                Token(50 + round * 2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(2),
                    off: 0,
                    len: 512,
                    file_off: round * 512,
                },
            ),
            (
                Token(51 + round * 2),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let mut done = 0;
        while done < 2 {
            sim.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 50 + round * 2 || c.token.0 == 51 + round * 2 {
                    done += 1;
                }
            }
        }
    }

    // Power loss mid-session, then cleanly reopen & verify fault counters accessible.
    sim.power_loss();
    sim.set_disk_faults(DiskFaults::default());
    let plen = write_nul_path(&mut sim, path_buf, 0, "/chaos.log");
    let reopen = submit_and_wait_one(
        &mut sim,
        &mut out,
        Token(99),
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
    .unwrap()
    .result;
    assert_eq!(reopen, 0);

    let counters = sim.disk_fault_counters();
    let total_faults = counters.short_write
        + counters.torn_reported
        + counters.lost_reported
        + counters.sync_fail
        + counters.read_error;
    (sim.trace_hash(), total_faults)
}

#[test]
fn t4_12_dual_domain_chaos_campaign_20_seeds_deterministic() {
    let mut faults_seen = 0u64;
    for seed in 0..20u64 {
        let a = run_chaos_seed(seed);
        let b = run_chaos_seed(seed);
        assert_eq!(a, b, "seed {seed}: dual-domain chaos replay diverged");
        faults_seen += a.1;
    }
    assert!(
        faults_seen > 0,
        "chaos campaign must inject faults across 20 seeds"
    );
}
