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
use longshore_common::sim::PlacedFault;
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

#[test]
fn a_short_direct_write_resumes_from_its_last_whole_block() {
    let mut rig = rig();
    let flags = rw_create().with_empty(true).with_direct(true);
    let f = rig.open("/direct-short.dat", flags);
    let len = 3 * BLOCK;
    for round in 0..4u64 {
        let data = bytes(round as u8, len as usize);
        let at = round * u64::from(len);
        let mark = rig.io().submitted();
        place(&mut rig, 1, PlacedFault::ShortWrite);
        assert_eq!(rig.write(f, &data, at), Ok(u64::from(len)));
        let w = writes(rig.io().since(mark));
        assert_eq!(w.len(), 2, "{w:?}");
        let (from, off, rest) = w[1];
        assert!(
            from.is_multiple_of(u64::from(BLOCK)),
            "resumed at a block: {w:?}"
        );
        assert_eq!(u64::from(off), from - at, "from the same buffer: {w:?}");
        assert_eq!(rest, len - off, "{w:?}");
        assert!(off < len, "{w:?}");
        assert_eq!(rig.read(f, at, len), (Ok(u64::from(len)), data));
    }
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

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
    // Less than a sector, cut: nothing written.
    place(&mut rig, 1, PlacedFault::ShortWrite);
    assert_eq!(rig.write(f, &[1; 100], 0), Err(FileError::NoProgress));
    // A sector, then nothing of the rest.
    place(&mut rig, 1, PlacedFault::ShortWrite);
    place(&mut rig, 2, PlacedFault::ShortWrite);
    let stalled = FileError::Partial {
        done: 512,
        kind: ErrorKind::NoProgress,
    };
    assert_eq!(rig.write(f, &[2; 600], 0), Err(stalled));
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
