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
    Limits::BASE
        .with_sockets_max(8)
        .with_buffers(4)
        .with_buffer_size(BLOCK)
        .with_small_buffers(4)
        .with_small_buffer_size(256)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(BLOCK)
        .with_block_size(BLOCK)
        .with_files_max(4)
        .with_ops_max(16)
}

/// A simulator with both network and disk faults switched on for `seed`.
fn chaos_sim(lim: Limits, seed: u64) -> SimIo {
    let net_faults = Faults::NONE
        .with_reset(50_000)
        .with_short_send(200_000)
        .with_partial_recv(200_000)
        .with_delay(150_000)
        .with_bit_flip(20_000);
    let disk_faults = DiskFaults::NONE
        .with_short_write(100_000)
        .with_torn_reported(50_000)
        .with_lost_reported(50_000)
        .with_sync_fail(50_000)
        .with_read_error(30_000);
    let mut sim = SimIo::new(lim, seed, net_faults);
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(6)
            .with_max_dirs(8)
            .with_max_bytes_per_file((BLOCK as usize) * 8)
            .with_max_name_len(64)
            .with_write_log_entries(8)
            .with_write_log_bytes((BLOCK as usize) * 8)
            .with_read_error_sectors(8)
            .with_sector_size(512),
    );
    sim.set_disk_faults(disk_faults);
    sim
}

/// Tries to connect `Sock(1)` to `Sock(2)`; network faults may prevent it.
/// Returns whether both sides completed.
fn try_connect(sim: &mut SimIo, out: &mut Completions) -> bool {
    let addr = sim
        .listen(Sock::new(0), "127.0.0.1:9200".parse().unwrap())
        .unwrap();
    sim.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    sim.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    for _ in 0..50 {
        sim.tick(true, out).unwrap();
        let mut saw_a = false;
        let mut saw_c = false;
        while let Some(c) = out.pop() {
            if c.token().get() == 1 && c.result() >= 0 {
                saw_a = true;
            }
            if c.token().get() == 2 && c.result() == 0 {
                saw_c = true;
            }
        }
        if saw_a && saw_c {
            return true;
        }
        if out.is_empty() {
            break;
        }
    }
    false
}

/// Opens `/chaos.log` read-write-create into `FileSlot(0)`; returns the result.
fn open_chaos_log(sim: &mut SimIo, out: &mut Completions, path_buf: BufId, token: Token) -> i32 {
    let plen = write_nul_path(sim, path_buf, 0, "/chaos.log");
    submit_and_wait_one(
        sim,
        out,
        token,
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf,
            path_off: 0,
            path_len: plen,
            flags: OpenFlags::NONE
                .with_read(true)
                .with_write(true)
                .with_create(true),
        },
        1_000,
    )
    .unwrap()
    .result()
}

/// One network Send/Recv exchange. Network faults may fail or stall it; the
/// test checks that the disk chain beside it is unaffected, not the exchange.
fn exchange_round(sim: &mut SimIo, out: &mut Completions, round: u64) {
    write_pool_bytes(sim, BufId::new(0), 0, &[0x40 + round as u8; 512]);
    let _: Option<longshore::io::Completion> = submit_and_wait_one(
        sim,
        out,
        Token::new(10 + round * 4),
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: 512,
            zero_copy: false,
        },
        500,
    )
    .ok();
    let _: Option<longshore::io::Completion> = submit_and_wait_one(
        sim,
        out,
        Token::new(11 + round * 4),
        Op::Recv {
            sock: Sock::new(2),
            buf: BufId::new(1),
            off: 0,
            len: 512,
        },
        500,
    )
    .ok();
}

/// Appends 512 bytes for `round` via a `[FileWrite, FileSync]` chain and
/// waits for both to complete, whatever their results.
fn write_sync_round(sim: &mut SimIo, out: &mut Completions, round: u64) {
    write_pool_bytes(sim, BufId::new(2), 0, &[0x70 + round as u8; 512]);
    sim.submit_chain(&[
        (
            Token::new(50 + round * 2),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(2),
                off: 0,
                len: 512,
                file_off: round * 512,
            },
        ),
        (
            Token::new(51 + round * 2),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    let mut done = 0;
    while done < 2 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 50 + round * 2 || c.token().get() == 51 + round * 2 {
                done += 1;
            }
        }
    }
}

fn run_chaos_seed(seed: u64) -> (u64, u64) {
    let lim = chaos_limits();
    let mut sim = chaos_sim(lim, seed);
    let mut out = Completions::new(16);
    let connected = try_connect(&mut sim, &mut out);

    let path_buf = lim.small(0);
    assert_eq!(
        open_chaos_log(&mut sim, &mut out, path_buf, Token::new(3)),
        0
    );

    // Interleave network Send/Recv with disk Write+Sync chains.
    for round in 0..4u64 {
        if connected {
            exchange_round(&mut sim, &mut out, round);
        }
        write_sync_round(&mut sim, &mut out, round);
    }

    // Power loss mid-session, then cleanly reopen & verify fault counters accessible.
    sim.power_loss();
    sim.set_disk_faults(DiskFaults::default());
    assert_eq!(
        open_chaos_log(&mut sim, &mut out, path_buf, Token::new(99)),
        0
    );

    let counters = sim.disk_fault_counters();
    let total_faults = counters.short_write()
        + counters.torn_reported()
        + counters.lost_reported()
        + counters.sync_fail()
        + counters.read_error();
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
