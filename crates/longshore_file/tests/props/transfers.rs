// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Whole transfers (C6) and alignment (C9), for any shape: a write cut
//! short any number of times ends whole and reads back; on a direct-I/O
//! file a transfer is refused `Unaligned` exactly when its place in the
//! extent, its length or its file offset is off a block, and a punch
//! everywhere exactly when its range is.

use super::common::{between, sim};
use hegel::generators as gs;
use hegel::TestCase;
use longshore_common::io::Limits;
use longshore_common::sim::{PlacedFault, SimIo};
use longshore_file::check::{self, bytes, rw_create, Rig, BLOCK, EXTENT};
use longshore_file::{ErrorKind, FileError};

fn rig(sector: u32) -> Rig<SimIo> {
    Rig::new(
        &mut |l: Limits| sim(l, sector),
        check::limits(),
        check::sizes(),
    )
}

#[hegel::test(derandomize = true, database = None::<String>)]
fn a_write_cut_short_any_number_of_times_ends_whole(tc: TestCase) {
    let sector = tc.draw(gs::sampled_from(vec![512u32, 4096]));
    let mut rig = rig(sector);
    let f = rig.open("/cut.dat", rw_create().with_empty(true));
    let len = between(&tc, 1, EXTENT);
    let at = between(&tc, 0, EXTENT - len);
    let off = u64::from(between(&tc, 0, 60_000));
    let cuts = between(&tc, 0, 5);
    let next = rig.io().submitted() + 1;
    for k in 0..u64::from(cuts) {
        rig.io()
            .inner_mut()
            .place_disk_fault(next + k, PlacedFault::ShortWrite);
    }
    let data = bytes(tc.draw(gs::integers::<u8>()), len as usize);
    let e = rig.extent();
    rig.fill(e, at, &data);
    let r = rig.run("write", |s, io, x| s.write_at(io, x, f, (e, at, len), off));
    rig.release(e);
    assert_eq!(r, Ok(u64::from(len)), "{cuts} cuts of {len} at {off}");
    burn(&mut rig, next + u64::from(cuts));
    assert_eq!(rig.read(f, off, len), (Ok(u64::from(len)), data));
    // A read past the end gives what there is.
    let past = between(&tc, 0, EXTENT);
    let (n, _) = rig.read(f, off + u64::from(len) - 1, past.max(1));
    assert_eq!(n, Ok(1));
    assert_eq!(rig.close(f), Ok(0));
}

#[hegel::test(derandomize = true, database = None::<String>)]
fn a_direct_write_cut_short_ends_whole_or_without_progress(tc: TestCase) {
    let mut rig = rig(512);
    let flags = rw_create().with_empty(true).with_direct(true);
    let f = rig.open("/cut-direct.dat", flags);
    let blocks = between(&tc, 1, EXTENT / BLOCK);
    let len = blocks * BLOCK;
    let off = u64::from(between(&tc, 0, 10) * BLOCK);
    let cuts = between(&tc, 0, 3);
    let next = rig.io().submitted() + 1;
    for k in 0..u64::from(cuts) {
        rig.io()
            .inner_mut()
            .place_disk_fault(next + k, PlacedFault::ShortWrite);
    }
    let data = bytes(7, len as usize);
    let e = rig.extent();
    rig.fill(e, 0, &data);
    let r = rig.run("write", |s, io, x| s.write_at(io, x, f, (e, 0, len), off));
    rig.release(e);
    burn(&mut rig, next + u64::from(cuts));
    match r {
        Ok(n) => {
            assert_eq!(n, u64::from(len));
            assert_eq!(rig.read(f, off, len), (Ok(n), data));
        }
        // A cut inside a block that cannot be resumed from a whole one.
        Err(FileError::NoProgress) => {}
        Err(FileError::Partial {
            done,
            kind: ErrorKind::NoProgress,
        }) => assert!(done.is_multiple_of(u64::from(BLOCK)) && done < u64::from(len)),
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.close(f), Ok(0));
}

/// Faults placed up to submission `until` that a write did not take go
/// to statuses of a missing path, which fail, rather than to what comes
/// next.
fn burn(rig: &mut Rig<SimIo>, until: u64) {
    while rig.io().submitted() < until {
        let left = rig.status("/nothing");
        assert!(left.is_err(), "{left:?}");
    }
}

fn aligned(v: u64) -> bool {
    v.is_multiple_of(u64::from(BLOCK))
}

#[hegel::test(derandomize = true, database = None::<String>)]
fn direct_io_is_refused_exactly_off_a_block(tc: TestCase) {
    let mut rig = rig(512);
    let f = rig.open("/align.dat", rw_create().with_direct(true));
    let pick = |c: &TestCase, most: u32| {
        if c.draw(gs::booleans()) {
            between(c, 0, most / BLOCK) * BLOCK
        } else {
            between(c, 0, most)
        }
    };
    let len = pick(&tc, EXTENT).max(1);
    let at = pick(&tc, EXTENT - len);
    let off = u64::from(pick(&tc, 40_000));
    let read = tc.draw(gs::booleans());
    let ok = aligned(u64::from(at)) && aligned(u64::from(len)) && aligned(off);
    let e = rig.extent();
    let range = (e, at, len);
    if ok {
        let r = if read {
            rig.run("read", |s, io, x| s.read_at(io, x, f, range, off))
        } else {
            rig.run("write", |s, io, x| s.write_at(io, x, f, range, off))
        };
        assert!(r.is_ok(), "{r:?}");
    } else if read {
        rig.refused("read", FileError::Unaligned, |s, io, x| {
            s.read_at(io, x, f, range, off)
        });
    } else {
        rig.refused("write", FileError::Unaligned, |s, io, x| {
            s.write_at(io, x, f, range, off)
        });
    }
    // A punch, on any file, is whole blocks.
    let (p_off, p_len) = (
        u64::from(pick(&tc, 20_000)),
        u64::from(pick(&tc, 9000).max(1)),
    );
    if aligned(p_off) && aligned(p_len) {
        let r = rig.run("punch", |s, io, _| s.punch(io, f, (p_off, p_len)));
        assert_eq!(r, Ok(0));
    } else {
        rig.refused("punch", FileError::Unaligned, |s, io, _| {
            s.punch(io, f, (p_off, p_len))
        });
    }
    rig.release(e);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
