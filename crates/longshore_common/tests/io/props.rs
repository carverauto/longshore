// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) for the plain data of the I/O layer: a connect's
//! segment sizes packed into its result, the completion buffer, and the
//! layout of the buffer pool.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_common::io::{BufId, Completion, Completions, Limits, Segment, Token};
use std::collections::VecDeque;

/// A measured connect's result is positive, so it cannot be taken for an
/// error or for an unmeasured connect, and reads back as what was measured:
/// sizes capped at 65,535, the path limit to the eight bytes below.
#[hegel::test]
fn a_segment_survives_its_trip_through_a_connect_result(tc: TestCase) {
    // Mostly sizes a connection can have, sometimes anything at all.
    let small = gs::integers::<u32>().max_value(70_000);
    let size = if tc.draw(gs::booleans()) {
        tc.draw(small)
    } else {
        tc.draw(gs::integers::<u32>())
    };
    let small = gs::integers::<u32>().max_value(70_000);
    let path_limit = if tc.draw(gs::booleans()) {
        tc.draw(small)
    } else {
        tc.draw(gs::integers::<u32>())
    };
    let r = Segment { size, path_limit }.encode();
    assert!(r >= 0, "{r} would read as an error");
    let want = (size != 0).then_some(Segment {
        size: size.min(65_535),
        path_limit: path_limit.min(65_535) / 8 * 8,
    });
    assert_eq!(Segment::decode(r), want);
    if size != 0 {
        assert!(r > 0, "0 is an unmeasured connect");
    }
}

/// A failed connect and an unmeasured one carry no segment.
#[hegel::test]
fn a_result_that_is_not_positive_has_no_segment(tc: TestCase) {
    let r = tc.draw(gs::integers::<i32>().max_value(0));
    assert_eq!(Segment::decode(r), None);
}

/// Any positive result reads as a size below 65,536 and a limit that is a
/// multiple of eight, or as nothing: decoding never invents a size of zero.
#[hegel::test]
fn decoding_any_result_gives_a_usable_segment_or_none(tc: TestCase) {
    let r = tc.draw(gs::integers::<i32>());
    if let Some(s) = Segment::decode(r) {
        assert!(r > 0);
        assert!((1..=65_535).contains(&s.size));
        assert_eq!(s.path_limit % 8, 0);
        // Encoding caps the limit at 65,535, which is 13 bits of eights. A
        // result with more than that set is one no encoder here produced.
        if r < 1 << 29 {
            assert_eq!(s.encode(), r, "and encodes back to the same result");
        }
    }
}

/// The completion buffer is a queue of fixed capacity: completions come out
/// in the order they went in, and its counts are the queue's.
#[hegel::test]
fn completions_come_out_in_the_order_they_went_in(tc: TestCase) {
    let capacity = tc.draw(gs::integers::<usize>().max_value(8));
    let mut buf = Completions::new(capacity);
    let mut model: VecDeque<Completion> = VecDeque::new();
    let steps = tc.draw(gs::integers::<usize>().max_value(60));
    for _ in 0..steps {
        if tc.draw(gs::booleans()) && !buf.is_full() {
            let c = Completion {
                token: Token(tc.draw(gs::integers::<u64>())),
                result: tc.draw(gs::integers::<i32>()),
            };
            buf.push(c);
            model.push_back(c);
        } else {
            assert_eq!(buf.pop(), model.pop_front());
        }
        assert_eq!(buf.len(), model.len());
        assert_eq!(buf.is_empty(), model.is_empty());
        assert_eq!(buf.is_full(), model.len() == capacity);
    }
    while let Some(c) = buf.pop() {
        assert_eq!(Some(c), model.pop_front());
    }
    assert!(model.is_empty());
}

fn limits(tc: &TestCase) -> Limits {
    let block_shift = tc.draw(gs::integers::<u32>().max_value(3));
    let block_size = 4096u32 << block_shift;
    let aligned_buffers = tc.draw(gs::integers::<u32>().max_value(6));
    let mult = tc.draw(gs::integers::<u32>().min_value(1).max_value(3));
    Limits {
        sockets_max: 1,
        buffers: tc.draw(gs::integers::<u32>().max_value(6)),
        buffer_size: tc.draw(gs::integers::<u32>().min_value(1).max_value(5000)),
        small_buffers: tc.draw(gs::integers::<u32>().max_value(6)),
        small_buffer_size: tc.draw(gs::integers::<u32>().min_value(1).max_value(700)),
        files_max: 0,
        aligned_buffers,
        aligned_buffer_size: block_size * mult,
        block_size,
        ops_max: 1,
    }
}

/// Buffers of all three classes lie in order in the pool, with optional
/// alignment gap before the aligned class and `block_size` head padding.
#[hegel::test]
fn buffers_tile_the_pool_for_any_limits(tc: TestCase) {
    let l = limits(&tc);
    let mut at = 0usize;
    for id in 0..l.buffer_count() {
        if id == l.buffers + l.small_buffers {
            at = at.next_multiple_of(l.block_size as usize);
        }
        let (start, len) = l.span(BufId(id));
        assert_eq!(start, at, "buffer {id} starts where expected");
        let want = if id < l.buffers {
            l.buffer_size
        } else if id < l.buffers + l.small_buffers {
            l.small_buffer_size
        } else {
            assert_eq!(start % l.block_size as usize, 0);
            l.aligned_buffer_size
        };
        assert_eq!(len, want as usize);
        assert_eq!(l.range(BufId(id), 0, want), start..start + len);
        assert_eq!(
            l.is_aligned_buf(BufId(id)),
            id >= l.buffers + l.small_buffers
        );
        at += len;
    }
    if l.buffer_count() == 0 {
        assert_eq!(l.pool_bytes(), 0);
    } else {
        assert_eq!(at + l.block_size as usize, l.pool_bytes());
    }
    for i in 0..l.small_buffers {
        assert_eq!(l.small(i), BufId(l.buffers + i));
    }
    for i in 0..l.aligned_buffers {
        assert_eq!(l.aligned(i), BufId(l.buffers + l.small_buffers + i));
    }
}

/// Two buffers borrowed at once are those two buffers, in the order asked.
#[hegel::test]
fn a_pair_is_the_two_buffers_asked_for(tc: TestCase) {
    let l = limits(&tc);
    if l.buffer_count() < 2 {
        return;
    }
    let pick = || gs::integers::<u32>().max_value(l.buffer_count() - 1);
    let (a, b) = (tc.draw(pick()), tc.draw(pick()));
    if a == b {
        return;
    }
    let mut pool = vec![0u8; l.pool_bytes()];
    {
        let (x, y) = l.pair(&mut pool, BufId(a), BufId(b));
        x.fill(1);
        y.fill(2);
    }
    for id in 0..l.buffer_count() {
        let (start, len) = l.span(BufId(id));
        let want = if id == a {
            1
        } else if id == b {
            2
        } else {
            0
        };
        assert!(
            pool[start..start + len].iter().all(|v| *v == want),
            "buffer {id} after writing through the pair ({a}, {b})"
        );
    }
}
