// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

const LIMITS: Limits = Limits {
    sockets_max: 1,
    buffers: 2,
    buffer_size: 1000,
    small_buffers: 3,
    small_buffer_size: 10,
    ops_max: 1,
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
