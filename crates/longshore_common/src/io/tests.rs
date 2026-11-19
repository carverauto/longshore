// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[allow(clippy::needless_update)]
const LIMITS: Limits = Limits {
    sockets_max: 1,
    buffers: 2,
    buffer_size: 1000,
    small_buffers: 3,
    small_buffer_size: 10,
    ops_max: 1,
    ..Limits::BASE
};

#[test]
fn buffers_of_both_sizes_tile_the_pool() {
    assert_eq!((LIMITS.buffer_count(), LIMITS.pool_bytes()), (5, 2030));
    let spans: Vec<_> = (0..5).map(|i| LIMITS.span(BufId(i))).collect();
    assert_eq!(
        spans,
        [(0, 1000), (1000, 1000), (2000, 10), (2010, 10), (2020, 10)]
    );
    assert_eq!(LIMITS.small(2), BufId(4));
    assert_eq!(LIMITS.range(BufId(3), 4, 6), 2014..2020);
}

#[test]
fn a_pair_borrows_two_buffers_in_the_order_asked() {
    let mut pool: Vec<u8> = (0..LIMITS.pool_bytes()).map(|i| (i / 10) as u8).collect();
    for (a, b) in [(0, 1), (1, 0), (0, 4), (4, 0), (2, 3), (3, 2)] {
        let (a_at, a_len) = LIMITS.span(BufId(a));
        let (b_at, b_len) = LIMITS.span(BufId(b));
        let (x, y) = LIMITS.pair(&mut pool, BufId(a), BufId(b));
        assert_eq!((x.len(), y.len()), (a_len, b_len));
        assert_eq!(
            (x[0], y[0]),
            ((a_at / 10) as u8, (b_at / 10) as u8),
            "pair ({a}, {b})"
        );
    }
}

#[test]
#[should_panic(expected = "buffer range out of bounds")]
fn a_range_past_a_small_buffer_is_refused() {
    LIMITS.range(BufId(2), 5, 6);
}

#[test]
#[should_panic(expected = "buffer out of range")]
fn a_buffer_past_the_pool_is_refused() {
    LIMITS.span(BufId(5));
}

#[test]
fn a_range_may_end_exactly_at_its_buffers_end() {
    assert_eq!(LIMITS.range(BufId(2), 4, 6), 2004..2010);
    assert_eq!(LIMITS.range(BufId(2), 10, 0), 2010..2010);
    assert_eq!(LIMITS.range(BufId(1), 0, 1000), 1000..2000);
}

#[test]
#[should_panic(expected = "buffer range out of bounds")]
fn a_range_whose_end_overflows_is_refused() {
    LIMITS.range(BufId(0), u32::MAX, 2);
}

#[test]
#[should_panic(expected = "small buffer out of range")]
fn a_small_buffer_past_the_last_is_refused() {
    LIMITS.small(3);
}

#[test]
#[should_panic(expected = "buf_pair needs two distinct pool buffers")]
fn a_pair_of_the_same_buffer_is_refused() {
    let mut pool = vec![0u8; LIMITS.pool_bytes()];
    LIMITS.pair(&mut pool, BufId(1), BufId(1));
}

#[test]
fn completions_report_full_and_empty_at_their_capacity() {
    let mut c = Completions::new(2);
    assert!(c.is_empty() && !c.is_full() && c.pop().is_none());
    for t in [5, 6] {
        c.push(Completion {
            token: Token(t),
            result: -(t as i32),
        });
    }
    assert!(c.is_full() && c.len() == 2);
    assert_eq!(c.pop().map(|x| (x.token.0, x.result)), Some((5, -5)));
    assert_eq!(c.pop().map(|x| (x.token.0, x.result)), Some((6, -6)));
    assert!(c.is_empty() && c.pop().is_none());
    // No room at all is both full and empty.
    let none = Completions::new(0);
    assert!(none.is_full() && none.is_empty());
}
