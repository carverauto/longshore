// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-06`–`T3-08`: `SimIo` trace parity with zero
//! `DiskFaults`, `power_loss` + `PlacedFault` op continuity, and chained short write.

use super::super::harness::{drive_until, write_pool_bytes};
use super::{
    disk_config, file_limits, open_at, read_only, read_op, result_of, run_op, rw_create, sync_op,
    sync_root_dir, write_op,
};
use longshore::io::{
    BufId, Completion, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token, ECANCELED,
};
use longshore::sim::{DiskFaultCounters, DiskFaults, Faults, PlacedFault, SimIo};

const EIO: i32 = 5;

/// Listens on `Sock(0)` at `addr`, accepts `Sock(2)` and connects `Sock(1)`.
fn accept_and_connect(sim: &mut SimIo, out: &mut Completions, addr: &str) {
    let addr = sim.listen(Sock(0), addr.parse().unwrap()).unwrap();
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
    let _: Vec<Completion> = drive_until(sim, out, 1000, |v| v.len() == 2).unwrap();
}

/// A network pair plus a chained file write and sync with zero disk faults;
/// returns the trace hash and the disk fault counters.
fn run_mixed_zero_faults(seed: u64) -> (u64, DiskFaultCounters) {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, seed, Faults::default());
    sim.setup_disk(disk_config());
    sim.set_disk_faults(DiskFaults::default());
    let mut out = Completions::new(16);

    accept_and_connect(&mut sim, &mut out, "192.0.2.36:9306");
    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            3,
            FileSlot(0),
            "/t3_06.dat",
            rw_create()
        ),
        0
    );
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x42; 512]);
    sim.submit_chain(&[
        (Token(4), write_op(FileSlot(0), BufId(0), 512, 0)),
        (Token(5), sync_op(FileSlot(0), true)),
    ])
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();
    (sim.trace_hash(), sim.disk_fault_counters())
}

#[test]
fn t3_06_simio_trace_parity_with_zero_disk_faults() {
    let (h1, c1) = run_mixed_zero_faults(77);
    let (h2, c2) = run_mixed_zero_faults(77);
    assert_eq!(
        h1, h2,
        "same seed with DiskFaults=0 must give identical trace hash"
    );
    assert_eq!(c1, Default::default());
    assert_eq!(c2, Default::default());
}

/// Queues a recv and a second file write, then loses power: neither may
/// complete.
fn power_loss_drops_inflight(sim: &mut SimIo, out: &mut Completions) {
    sim.submit(
        Token(10),
        Op::Recv {
            sock: Sock(2),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    write_pool_bytes(sim, BufId(0), 0, &[0x22; 512]);
    sim.submit(Token(11), write_op(FileSlot(0), BufId(0), 512, 512))
        .unwrap();

    sim.power_loss();
    sim.tick(false, out).unwrap();
    assert!(
        out.is_empty(),
        "power_loss must deliver zero completions for in-flight ops"
    );
}

#[test]
fn t3_07_simio_power_loss_and_placed_fault_op_number_continuity() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(
        limits,
        307,
        Faults {
            delay: 100_000,
            ..Default::default()
        },
    );
    sim.setup_disk(disk_config());
    sim.set_disk_faults(DiskFaults::default());
    let mut out = Completions::new(16);
    sim.place_disk_fault(6, PlacedFault::Fail);

    accept_and_connect(&mut sim, &mut out, "192.0.2.37:9307");
    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            3,
            FileSlot(0),
            "/t3_07.dat",
            rw_create()
        ),
        0
    );
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x11; 512]);
    let write = write_op(FileSlot(0), BufId(0), 512, 0);
    assert_eq!(run_op(&mut sim, &mut out, 4, write, 1000), 512);
    let sync = sync_op(FileSlot(0), true);
    assert_eq!(run_op(&mut sim, &mut out, 5, sync, 1000), 0);

    power_loss_drops_inflight(&mut sim, &mut out);

    let rw = OpenFlags {
        read: true,
        write: true,
        ..Default::default()
    };
    assert_eq!(
        open_at(&mut sim, &mut out, 12, FileSlot(0), "/t3_07.dat", rw),
        0
    );
    let op6 = run_op(
        &mut sim,
        &mut out,
        13,
        read_op(FileSlot(0), BufId(2), 512, 0),
        1000,
    );
    assert_eq!(op6, -EIO);
}

#[test]
fn t3_08_chained_write_sync_short_write_and_power_loss() {
    let limits = file_limits(4, 2, 4096);
    let mut sim = SimIo::new(limits, 308, Faults::default());
    sim.setup_disk(disk_config());
    let mut out = Completions::new(16);

    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            1,
            FileSlot(0),
            "/t3_08.dat",
            rw_create()
        ),
        0
    );
    sync_root_dir(&mut sim, &mut out, 10, FileSlot(1));
    sim.place_disk_fault(5, PlacedFault::ShortWrite);
    write_pool_bytes(&mut sim, BufId(0), 0, &[0x77; 4096]);
    sim.submit_chain(&[
        (Token(2), write_op(FileSlot(0), BufId(0), 4096, 0)),
        (Token(3), sync_op(FileSlot(0), true)),
    ])
    .unwrap();
    let cs = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();
    let w = result_of(&cs, 2);
    let s = result_of(&cs, 3);
    assert!(
        w > 0 && w < 4096 && w % 512 == 0,
        "short write must be sector-aligned: {w}"
    );
    assert_eq!(s, -ECANCELED, "sync after short write must be cancelled");

    sim.power_loss();
    assert_eq!(
        open_at(
            &mut sim,
            &mut out,
            4,
            FileSlot(0),
            "/t3_08.dat",
            read_only()
        ),
        0
    );
    let r = run_op(
        &mut sim,
        &mut out,
        5,
        read_op(FileSlot(0), BufId(1), 4096, 0),
        1000,
    );
    assert!(r >= 0 && r <= w);
}
