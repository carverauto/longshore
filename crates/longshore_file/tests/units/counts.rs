// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Every backend reports a transfer's bytes as an `i32`: a transfer of
//! more is refused `TooLarge` at the call, submitting nothing and taking
//! no slot and no hold (contract C9, C11).

use crate::rig::{base, rw, Rig};
use longshore_common::io::extents::Extent;
use longshore_common::io::Limits;
use longshore_file::{FileError, FileSizes, Span, Step, Ticket};

/// Bytes of the one block-aligned buffer: 512 spans of it are 2 GiB.
const BIG: u32 = 4 << 20;

/// The most bytes one transfer may move.
const MOST: u32 = i32::MAX.unsigned_abs();

fn big() -> Limits {
    base()
        .with_small_buffer_size(16 << 10)
        .with_aligned_buffers(1)
        .with_aligned_buffer_size(BIG)
}

/// `n` spans of the whole of `e`.
fn whole(e: Extent, n: usize) -> Vec<Span> {
    let s = Span::Extent {
        extent: e,
        at: 0,
        len: BIG,
    };
    vec![s; n]
}

/// The rig's set and backend as a refusal must leave them.
fn untouched(r: &Rig, logged: usize, e: Extent) {
    assert_eq!(r.io.log.len(), logged, "nothing submitted");
    assert_eq!(r.files.usage().ops(), 0, "no slot taken");
    assert_eq!(r.x.holds(e), Some(1), "no hold taken");
}

#[test]
fn a_gathered_write_of_more_than_i32_max_bytes_is_refused_at_the_call() {
    let mut r = Rig::with_base(big(), FileSizes::new(1, 2).with_spans_max(1024));
    let f = r.open("/big", rw());
    let e = r.x.take().unwrap();
    let logged = r.io.log.len();
    // 2^31 bytes, one more than an `i32` counts.
    let spans = whole(e, 512);
    let w = r.files.write_spans(&mut r.io, &mut r.x, f, &spans, 0);
    assert_eq!(w, Err(FileError::TooLarge));
    untouched(&r, logged, e);

    // As a step, the sequence is refused whole, and the tickets of the
    // steps before it are left naming nothing.
    let steps = [
        Step::Sync {
            file: f,
            data_only: true,
        },
        Step::WriteSpans {
            file: f,
            spans: &spans,
            file_off: 0,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    let s = r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t);
    assert_eq!(s, Err(FileError::TooLarge));
    assert_eq!(t, [Ticket::NONE; 2], "no ticket of a refused sequence");
    untouched(&r, logged, e);

    // `i32::MAX` bytes exactly are taken; power is lost before it runs.
    let mut most = whole(e, 512);
    most[511] = Span::Extent {
        extent: e,
        at: 0,
        len: BIG - 1,
    };
    assert_eq!(511 * u64::from(BIG) + u64::from(BIG - 1), u64::from(MOST));
    assert!(r
        .files
        .write_spans(&mut r.io, &mut r.x, f, &most, 0)
        .is_ok());
    assert_eq!(r.files.usage().ops(), 1);
    r.io.power_loss();
    r.files.power_lost(&mut r.x);
    assert_eq!(r.x.holds(e), Some(1));
}

#[test]
fn a_read_or_write_of_more_than_i32_max_bytes_is_refused_at_the_call() {
    // One buffer of 2 GiB and a block: its pages are never touched.
    let size = MOST + 1 + 4096;
    let l = base()
        .with_aligned_buffers(1)
        .with_aligned_buffer_size(size);
    let mut r = Rig::with_base(l, FileSizes::new(1, 2));
    let f = r.open("/big", rw());
    let e = r.x.take().unwrap();
    let logged = r.io.log.len();
    let range = (e, 0, MOST + 1);
    let w = r.files.write_at(&mut r.io, &mut r.x, f, range, 0);
    assert_eq!(w, Err(FileError::TooLarge));
    let rd = r.files.read_at(&mut r.io, &mut r.x, f, range, 0);
    assert_eq!(rd, Err(FileError::TooLarge));
    untouched(&r, logged, e);
}
