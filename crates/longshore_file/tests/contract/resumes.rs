// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Whole transfers (C6) and broken chains (C7), on the simulator, whose
//! disk can cut a write short: a short write is resumed for the rest under
//! its ticket, on a direct-I/O file from the last whole block; an error
//! after some bytes is `Partial`; a write of nothing is `NoProgress`; a
//! sequence whose chain a short write broke goes on, in order.

use super::sim::{place, rig};
use longshore_common::io::Op;
use longshore_common::sim::{PlacedFault, SimIo};
use longshore_file::check::{bytes, rw_create, tickets, Kind, Submitted, BLOCK};
use longshore_file::{ErrorKind, FileError, Step};

const EIO: i32 = 5;

/// The writes given to the backend from submission `mark` on: file
/// offset, place in the buffer, length.
fn writes(sent: impl Iterator<Item = Submitted>) -> Vec<(u64, u32, u32)> {
    sent.filter_map(|s| {
        if let Op::FileWrite {
            off, len, file_off, ..
        } = s.op()
        {
            Some((file_off, off, len))
        } else {
            None
        }
    })
    .collect()
}

#[test]
fn a_short_write_is_resumed_for_the_rest() {
    let mut rig = rig();
    let f = rig.open("/short.dat", rw_create().with_empty(true));
    let data = bytes(1, 10_000);
    let mark = rig.io().submitted();
    place(&mut rig, 1, PlacedFault::ShortWrite);
    assert_eq!(rig.write(f, &data, 0), Ok(10_000));
    let w = writes(rig.io().since(mark));
    assert_eq!(w.len(), 2, "{w:?}");
    let cut = w[1].0;
    assert!(cut > 0 && cut.is_multiple_of(512), "{w:?}");
    assert_eq!(w[1], (cut, cut as u32, 10_000 - cut as u32));
    assert_eq!(rig.read(f, 0, 10_000), (Ok(10_000), data));
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// The result of the first completion from completion number `from` on.
fn first_result(rig: &mut longshore_file::check::Rig<SimIo>, from: u64) -> i32 {
    rig.io().result(from).unwrap().1
}

/// Cut short, a direct write is written again from its last whole block,
/// from the same buffer: past the first block the rest of it, inside it
/// all of it again from the same offset (C6).
#[test]
fn a_short_direct_write_resumes_from_its_last_whole_block() {
    let mut rig = rig();
    let flags = rw_create().with_empty(true).with_direct(true);
    let f = rig.open("/direct-short.dat", flags);
    let len = 4 * BLOCK;
    let (mut resumed, mut again) = (0, 0);
    for round in 0..16u64 {
        let data = bytes(round as u8, len as usize);
        let at = round * u64::from(len);
        let (mark, from) = (rig.io().submitted(), rig.io().completed());
        place(&mut rig, 1, PlacedFault::ShortWrite);
        assert_eq!(rig.write(f, &data, at), Ok(u64::from(len)));
        let cut = first_result(&mut rig, from).unsigned_abs();
        let w = writes(rig.io().since(mark));
        assert_eq!(w.len(), 2, "{w:?}");
        let off = cut / BLOCK * BLOCK;
        assert_eq!(w[1], (at + u64::from(off), off, len - off), "cut at {cut}");
        assert_eq!(rig.read(f, at, len), (Ok(u64::from(len)), data));
        if off == 0 {
            again += 1;
        } else {
            resumed += 1;
        }
    }
    assert!(resumed > 0 && again > 0, "{resumed} resumed, {again} again");
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// What C6 says a direct write of `len` bytes ends with when the backend
/// completes its attempts with `results`, in order: each resumes from
/// what was written rounded down to a block; an attempt that adds no
/// whole block counts, and the fifth such in a row ends `NoProgress`.
fn direct_outcome(len: u64, results: &[i32]) -> Result<u64, FileError> {
    let (mut done, mut stalls) = (0u64, 0);
    for r in results {
        let Ok(n) = u64::try_from(*r) else {
            let kind = ErrorKind::Io { errno: EIO };
            return if done > 0 {
                Err(FileError::Partial { done, kind })
            } else {
                Err(FileError::Io { errno: EIO })
            };
        };
        if done + n == len {
            return Ok(len);
        }
        let now = (done + n) / u64::from(BLOCK) * u64::from(BLOCK);
        if now > done {
            (done, stalls) = (now, 0);
        } else {
            stalls += 1;
            if stalls == 5 {
                let kind = ErrorKind::NoProgress;
                return if done > 0 {
                    Err(FileError::Partial { done, kind })
                } else {
                    Err(FileError::NoProgress)
                };
            }
        }
    }
    panic!("the write did not end: {results:?}");
}

/// An error after some bytes is `Partial` with how many. A direct write
/// cut inside a block four times in a row is written whole the fifth
/// time; cut five times it ends `NoProgress`, or `Partial` of
/// `NoProgress` after whole blocks. The simulator never cuts a buffered
/// write to nothing.
#[test]
fn an_error_after_some_bytes_is_partial_and_none_is_no_progress() {
    let mut rig = rig();
    let f = rig.open("/partial.dat", rw_create().with_empty(true));
    place(&mut rig, 1, PlacedFault::ShortWrite);
    place(&mut rig, 2, PlacedFault::Fail);
    match rig.write(f, &bytes(2, 2048), 0) {
        Err(FileError::Partial {
            done,
            kind: ErrorKind::Io { errno: EIO },
        }) => assert!(
            done > 0 && done < 2048 && done.is_multiple_of(512),
            "{done}"
        ),
        other => panic!("cut, then failed: {other:?}"),
    }
    let flags = rw_create().with_empty(true).with_direct(true);
    let d = rig.open("/partial-direct.dat", flags);
    // One block: every cut is inside it.
    let block = bytes(4, BLOCK as usize);
    for k in 1..=4 {
        place(&mut rig, k, PlacedFault::ShortWrite);
    }
    assert_eq!(
        rig.write(d, &block, 0),
        Ok(u64::from(BLOCK)),
        "four cuts, then whole"
    );
    for k in 1..=5 {
        place(&mut rig, k, PlacedFault::ShortWrite);
    }
    assert_eq!(
        rig.write(d, &block, 0),
        Err(FileError::NoProgress),
        "five cuts"
    );
    // Two blocks, six cuts: what C6 says of the cuts the disk made.
    let mut seen = [0; 3];
    for round in 0..24u64 {
        let (from, mark) = (rig.io().completed(), rig.io().submitted());
        for k in 1..=6 {
            place(&mut rig, k, PlacedFault::ShortWrite);
        }
        let r = rig.write(d, &bytes(5, 2 * BLOCK as usize), round * 8192);
        let results: Vec<i32> = (from..rig.io().completed())
            .map(|n| rig.io().result(n).unwrap().1)
            .collect();
        assert_eq!(
            r,
            direct_outcome(u64::from(2 * BLOCK), &results),
            "{results:?}"
        );
        seen[match r {
            Ok(_) => 0,
            Err(FileError::NoProgress) => 1,
            Err(_) => 2,
        }] += 1;
        // Cuts the write did not take go to statuses, which fail.
        while rig.io().submitted() < mark + 6 {
            assert!(rig.status("/nothing-here").is_err());
        }
    }
    assert!(seen[1] + seen[2] > 0 && seen[0] + seen[2] > 0, "{seen:?}");
    assert_eq!(rig.close(d), Ok(0));
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

#[test]
fn a_chain_broken_by_a_short_write_goes_on_in_order() {
    let mut rig = rig();
    let f = rig.open("/broken.dat", rw_create().with_empty(true));
    let (from, into) = (rig.extent(), rig.extent());
    let data = bytes(3, 10_000);
    rig.fill(from, 0, &data);
    let steps = [
        Step::Write {
            file: f,
            extent: from,
            at: 0,
            len: 10_000,
            file_off: 0,
        },
        Step::Sync {
            file: f,
            data_only: true,
        },
        Step::Read {
            file: f,
            extent: into,
            at: 0,
            len: 10_000,
            file_off: 0,
        },
    ];
    let mark = rig.io().submitted();
    place(&mut rig, 1, PlacedFault::ShortWrite);
    let mut ts = tickets::<3>();
    let n = rig.call(|s, io, x| s.sequence(io, x, &steps, &mut ts));
    assert_eq!(n, Ok(3));
    let got = ts.map(|t| rig.done(t));
    assert_eq!(got, [Ok(10_000), Ok(0), Ok(10_000)]);
    assert_eq!(
        rig.bytes(into, 0, 10_000),
        data,
        "the read came after the write"
    );
    let sent: Vec<Submitted> = rig.io().since(mark).collect();
    let kinds: Vec<Kind> = sent.iter().map(|s| s.describe().0).collect();
    // The chain as asked for, cut after the write; the rest of the write
    // and the steps after it as a new one.
    let want = [Kind::Write, Kind::Sync, Kind::Read];
    assert_eq!(kinds[..3], want, "{sent:?}");
    assert_eq!(kinds[3..], want, "{sent:?}");
    assert!(sent[3].chain().is_some() && sent[3].chain() != sent[0].chain());
    rig.release(from);
    rig.release(into);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}
