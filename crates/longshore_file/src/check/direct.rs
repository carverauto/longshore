// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Direct I/O, checked before submission (C9): whole blocks from extents
//! of the block-aligned class are taken and read back; a length, a file
//! offset or a place in the extent off a block, or an extent of another
//! class, is `Unaligned` on every backend, as a range, a span or a step;
//! a punch is whole blocks on every file. A file system that cannot open
//! a file for direct I/O (tmpfs) leaves the direct checks out.

use super::{bytes, fail, limits, rw_create, sizes, tickets, Rig, BLOCK};
use crate::{FileError, Span, Step};
use longshore_common::io::extents::Extents;
use longshore_common::io::{BufId, Io, Limits};

pub(super) fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    punch_is_whole_blocks(make, root);
    direct_io(make, root);
}

/// On a buffered file too, a punch off a block is `Unaligned`.
fn punch_is_whole_blocks<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let f = rig.open(&format!("{root}punch-blocks.dat"), rw_create());
    let off = FileError::Unaligned;
    rig.refused("off", off, |s, io, _| {
        s.punch(io, f, (100, u64::from(BLOCK)))
    });
    rig.refused("len", off, |s, io, _| s.punch(io, f, (0, 100)));
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

fn direct_io<IO: Io>(make: &mut impl FnMut(Limits) -> IO, root: &str) {
    let mut rig = Rig::new(make, limits(), sizes());
    let flags = rw_create().with_empty(true).with_direct(true);
    let f = match rig.try_open(&format!("{root}direct.dat"), flags) {
        Ok(f) => f,
        Err(FileError::Invalid { .. } | FileError::Unsupported) => return,
        Err(e) => fail(format_args!("a direct-I/O open: {e:?}")),
    };
    let block = u64::from(BLOCK);
    let data = bytes(6, 3 * BLOCK as usize);
    assert_eq!(rig.write(f, &data, block), Ok(3 * block));
    let (r, got) = rig.read(f, block, 3 * BLOCK);
    assert_eq!((r, got == data), (Ok(3 * block), true));
    // A read past the end ends at the end, on a block.
    assert_eq!(rig.read(f, 3 * block, 2 * BLOCK).0, Ok(block));
    unaligned_refused(&mut rig, f);
    assert_eq!(rig.close(f), Ok(0));
    rig.at_rest(0);
}

/// Every way off a block, as a range, a span and a step.
fn unaligned_refused<IO: Io>(rig: &mut Rig<IO>, f: crate::File) {
    let e = rig.extent();
    let off = FileError::Unaligned;
    rig.refused("len", off, |s, io, x| s.write_at(io, x, f, (e, 0, 100), 0));
    rig.refused("file offset", off, |s, io, x| {
        s.write_at(io, x, f, (e, 0, BLOCK), 512)
    });
    rig.refused("at", off, |s, io, x| {
        s.write_at(io, x, f, (e, 512, BLOCK), 0)
    });
    rig.refused("read len", off, |s, io, x| {
        s.read_at(io, x, f, (e, 0, 100), 0)
    });
    rig.refused("read at", off, |s, io, x| {
        s.read_at(io, x, f, (e, 512, BLOCK), 0)
    });
    let spans = [
        Span::Extent {
            extent: e,
            at: 0,
            len: BLOCK,
        },
        Span::Extent {
            extent: e,
            at: 512,
            len: BLOCK,
        },
    ];
    rig.refused("a span", off, |s, io, x| s.write_spans(io, x, f, &spans, 0));
    let steps = [Step::Write {
        file: f,
        extent: e,
        at: 0,
        len: 100,
        file_off: 0,
    }];
    let mut ts = tickets::<1>();
    rig.refused("a step", off, |s, io, x| s.sequence(io, x, &steps, &mut ts));
    rig.release(e);
    // An extent of the large class: not block-aligned memory.
    let mut large = Extents::new(BufId::new(0), 1);
    if let Some(wrong) = large.take() {
        rig.refused("another class", off, |s, io, _| {
            s.write_at(io, &mut large, f, (wrong, 0, BLOCK), 0)
        });
    }
}
