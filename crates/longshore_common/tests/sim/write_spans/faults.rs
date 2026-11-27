// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A gathered write meets the disk's faults as one write: a fault placed
//! on it fires once and is counted once, a rolled fault is drawn once for
//! all its spans, a file opened with `dsync` is synced once at its end,
//! and an error is never taken for the bytes of the spans before it.

use super::*;
use longshore_common::io::posix::errno::linux::EIO;
use longshore_common::sim::{DiskFaultCounters, DiskFaults, PlacedFault};

/// Three spans of a block each: of two aligned buffers and a page.
#[cfg(test)]
fn three(io: &mut SimIo) -> Op {
    let (a, page, b) = (LIMITS.aligned(0), LIMITS.page(1), LIMITS.aligned(1));
    for (buf, byte) in [(a, 0xA1), (page, 0xB2), (b, 0xC3)] {
        fill(io, buf, 0, BLOCK, byte);
    }
    gather(
        io,
        &[(a, 0, BLOCK), (page, 0, BLOCK), (b, 0, BLOCK)],
        3 * BLOCK,
        0,
    )
}

/// How many bytes the file has, as a read of all three blocks finds.
#[cfg(test)]
fn size(io: &mut SimIo, out: &mut Completions) -> i32 {
    let read = Op::FileRead {
        slot: FILE,
        buf: LIMITS.aligned(2),
        off: 0,
        len: 3 * BLOCK,
        file_off: 0,
    };
    run(io, out, read)
}

/// What one of the counters says.
type Count = fn(DiskFaultCounters) -> u64;

/// A fault placed on a gathered write fires once, and is counted once,
/// however many spans the write has.
#[test]
fn a_placed_fault_fires_once_for_a_gathered_write() {
    let all = (3 * BLOCK).cast_signed();
    let counted: [(PlacedFault, Count); 2] = [
        (PlacedFault::LostSilent, DiskFaultCounters::lost_silent),
        (PlacedFault::TornSilent, DiskFaultCounters::torn_silent),
    ];
    for (fault, count) in counted {
        let (mut io, mut out) = opened(false);
        // The open was the first file operation; the write is the second.
        io.place_disk_fault(2, fault);
        let write = three(&mut io);
        assert_eq!(run(&mut io, &mut out, write), all, "{fault:?}");
        assert_eq!(count(io.disk_fault_counters()), 1, "{fault:?}");
        if fault == PlacedFault::LostSilent {
            assert_eq!(size(&mut io, &mut out), 0, "a lost write wrote nothing");
        }
    }
}

/// The result of one gathered write under `faults` from seed `seed`, on
/// a file opened with `flags`, and the counters before and after it.
#[cfg(test)]
fn under(
    seed: u64,
    flags: OpenFlags,
    faults: DiskFaults,
) -> (i32, DiskFaultCounters, DiskFaultCounters) {
    let (mut io, mut out) = opened_with(seed, flags);
    io.set_disk_faults(faults);
    let before = io.disk_fault_counters();
    let write = three(&mut io);
    let res = run(&mut io, &mut out, write);
    (res, before, io.disk_fault_counters())
}

/// A fault drawn for a gathered write is drawn once for all of it: the
/// write is lost whole and says so, or is written whole; never written
/// in part with the error of a later span dropped.
#[test]
fn a_rolled_fault_after_the_first_span_is_not_swallowed() {
    let faults = DiskFaults::NONE.with_lost_reported(300_000);
    let (mut failed, mut whole) = (0, 0);
    for seed in 0..60 {
        let (res, before, after) = under(seed, OpenFlags::NONE, faults);
        let drawn = after.lost_reported() - before.lost_reported();
        match res {
            r if r == -EIO => failed += 1,
            r if r == (3 * BLOCK).cast_signed() => whole += 1,
            r => panic!("seed {seed}: a gathered write ended with {r}"),
        }
        let want = u64::from(res == -EIO);
        assert_eq!(drawn, want, "seed {seed}: faults counted for {res}");
    }
    assert!(failed > 0 && whole > 0, "{failed} failed, {whole} whole");
}

/// A gathered write to a file opened with `dsync` is synced once, at its
/// end: it is durable whole, or its sync failed and it says so.
#[test]
fn a_dsync_gathered_write_is_synced_once() {
    let faults = DiskFaults::NONE.with_sync_fail(300_000);
    let flags = OpenFlags::NONE.with_dsync(true);
    let (mut failed, mut whole) = (0, 0);
    for seed in 0..60 {
        let (res, before, after) = under(seed, flags, faults);
        let synced_bad = after.sync_fail() - before.sync_fail();
        match res {
            r if r == -EIO => {
                assert_eq!(synced_bad, 1, "seed {seed}");
                failed += 1;
            }
            r if r == (3 * BLOCK).cast_signed() => {
                assert_eq!(synced_bad, 0, "seed {seed}");
                whole += 1;
            }
            r => panic!("seed {seed}: a gathered write ended with {r}"),
        }
    }
    assert!(failed > 0 && whole > 0, "{failed} failed, {whole} whole");
}
