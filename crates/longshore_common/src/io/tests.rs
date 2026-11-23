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
    ..Limits::BASE
};

#[test]
fn buffers_of_both_sizes_tile_the_pool() {
    assert_eq!(
        (LIMITS.buffer_count(), LIMITS.pool_bytes()),
        (5, 2030 + 4096)
    );
    let spans: Vec<_> = (0..5).map(|i| LIMITS.span(BufId(i))).collect();
    assert_eq!(
        spans,
        [(0, 1000), (1000, 1000), (2000, 10), (2010, 10), (2020, 10)]
    );
    assert_eq!(LIMITS.small(2), BufId(4));
    assert_eq!(LIMITS.range(BufId(3), 4, 6), 2014..2020);
}

#[test]
fn aligned_buffers_start_on_block_boundary_with_head_padding() {
    let l = Limits {
        buffers: 1,
        buffer_size: 1000,
        small_buffers: 1,
        small_buffer_size: 10,
        aligned_buffers: 2,
        aligned_buffer_size: 8192,
        block_size: 4096,
        ops_max: 1,
        ..Limits::BASE
    };
    assert_eq!(l.buffer_count(), 4);
    assert_eq!(l.aligned_start(), 4096);
    assert_eq!(l.pool_bytes(), 4096 + 2 * 8192 + 4096);
    assert_eq!(l.aligned(0), BufId(2));
    assert_eq!(l.aligned(1), BufId(3));
    assert!(!l.is_aligned_buf(BufId(0)));
    assert!(!l.is_aligned_buf(BufId(1)));
    assert!(l.is_aligned_buf(BufId(2)));
    assert!(l.is_aligned_buf(BufId(3)));
    assert_eq!(l.span(l.aligned(0)), (4096, 8192));
    assert_eq!(l.span(l.aligned(1)), (4096 + 8192, 8192));
    assert_eq!(Limits::BASE.pool_bytes(), 0);
}

#[test]
#[should_panic(expected = "block_size must be a power of two >= 4096")]
fn invalid_block_size_panics_when_aligned_buffers_present() {
    let l = Limits {
        aligned_buffers: 1,
        aligned_buffer_size: 4096,
        block_size: 2048,
        ..Limits::BASE
    };
    let _: usize = l.pool_bytes();
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
        c.push(Completion::new(Token(t), -(t as i32)));
    }
    assert!(c.is_full() && c.len() == 2);
    assert_eq!(c.pop().map(|x| (x.token.0, x.result)), Some((5, -5)));
    assert_eq!(c.pop().map(|x| (x.token.0, x.result)), Some((6, -6)));
    assert!(c.is_empty() && c.pop().is_none());
    // No room at all is both full and empty.
    let none = Completions::new(0);
    assert!(none.is_full() && none.is_empty());
}

#[test]
fn completion_new_sets_final_with_no_buffer() {
    let c = Completion::new(Token(42), 17);
    assert_eq!(c.token, Token(42));
    assert_eq!(c.result, 17);
    assert!(!c.more);
    assert_eq!(c.buf, None);
}

#[test]
fn ring_buffers_tile_after_aligned_and_validate_invariants() {
    let l = Limits {
        sockets_max: 8,
        accept_range_start: 2,
        accept_range_len: 6,
        buffers: 1,
        buffer_size: 1000,
        small_buffers: 1,
        small_buffer_size: 10,
        aligned_buffers: 1,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ring_buffers: 2,
        ring_buffer_size: 256,
        ops_max: 1,
        ..Limits::BASE
    };
    assert_eq!(l.ring_base(), 3);
    assert_eq!(l.buffer_count(), 5);
    assert_eq!(l.ring(0), BufId(3));
    assert_eq!(l.ring(1), BufId(4));
    assert!(!l.is_ring_buf(BufId(2)));
    assert!(l.is_ring_buf(BufId(3)));
    assert!(l.is_ring_buf(BufId(4)));
    assert!(!l.is_ring_buf(BufId(5)));
    assert_eq!(l.span(l.ring(0)), (8192, 256));
    assert_eq!(l.span(l.ring(1)), (8192 + 256, 256));
    assert_eq!(l.pool_bytes(), 8192 + 512 + 4096);
}

#[test]
#[should_panic(expected = "ring_buffers must be <= 32_768")]
fn ring_buffers_above_32768_are_refused() {
    let l = Limits {
        ring_buffers: 32_769,
        ring_buffer_size: 64,
        ..Limits::BASE
    };
    l.validate();
}

#[test]
#[should_panic(expected = "ring_buffer_size must be > 0")]
fn zero_ring_buffer_size_is_refused() {
    let l = Limits {
        ring_buffers: 1,
        ring_buffer_size: 0,
        ..Limits::BASE
    };
    l.validate();
}

#[test]
#[should_panic(expected = "accept_range_start + accept_range_len must be <= sockets_max")]
fn accept_range_past_sockets_max_is_refused() {
    let l = Limits {
        sockets_max: 4,
        accept_range_start: 2,
        accept_range_len: 3,
        ..Limits::BASE
    };
    l.validate();
}
