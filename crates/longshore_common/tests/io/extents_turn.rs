// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Extents: a buffer is reused only when its last hold is released, and a
//! handle kept past that is refused. Turn: storage completions come first.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::extents::{ExtentError, Extents};
use longshore_common::io::turn::{Class, Turn};
use longshore_common::io::{BufId, Completion, Completions, Token};

#[global_allocator]
static ALLOC: Guard = Guard;

#[test]
fn a_buffer_is_free_again_only_after_its_last_hold() {
    let mut ex = Extents::new(BufId::new(10), 2);
    let frame = ex.take().unwrap();
    assert_eq!(
        (frame.buf(), ex.holds(frame), ex.free_count()),
        (BufId::new(10), Some(1), 1)
    );
    // A log write and two sends each hold it.
    for _ in 0..3 {
        ex.hold(frame).unwrap();
    }
    // The receiver, then the two sends, then the log: free only at the last.
    for left in [3, 2, 1] {
        assert_eq!(ex.release(frame), Ok(false));
        assert_eq!((ex.holds(frame), ex.free_count()), (Some(left), 1));
    }
    assert_eq!(ex.release(frame), Ok(true));
    assert_eq!((ex.free_count(), ex.is_live(frame)), (2, false));
}

#[test]
fn a_handle_kept_past_the_last_release_is_refused() {
    let mut ex = Extents::new(BufId::new(0), 1);
    let old = ex.take().unwrap();
    assert_eq!(ex.release(old), Ok(true));
    // The buffer's next user gets the same buffer under a new generation.
    let new = ex.take().unwrap();
    assert_eq!(new.buf(), old.buf());
    assert_ne!(new, old);
    assert_eq!(ex.hold(old), Err(ExtentError::Stale));
    assert_eq!(ex.release(old), Err(ExtentError::Stale));
    assert_eq!((ex.holds(old), ex.holds(new)), (None, Some(1)));
    // The stale handle did not touch the new user's hold.
    assert_eq!(ex.release(new), Ok(true));
}

#[test]
fn taking_from_an_empty_range_gives_none_and_nothing_allocates() {
    let mut ex = Extents::new(BufId::new(4), 3);
    sealed(|| {
        let all = [ex.take().unwrap(), ex.take().unwrap(), ex.take().unwrap()];
        assert!(ex.take().is_none());
        let mut bufs = all.map(|e| e.buf().get());
        bufs.sort_unstable();
        assert_eq!(bufs, [4, 5, 6]);
        for e in all {
            assert_eq!(ex.release(e), Ok(true));
        }
        assert_eq!(ex.free_count(), 3);
    });
}

#[test]
fn hold_counts_stop_at_their_limit() {
    let mut ex = Extents::new(BufId::new(0), 1);
    let e = ex.take().unwrap();
    for _ in 1..u16::MAX {
        ex.hold(e).unwrap();
    }
    assert_eq!(ex.hold(e), Err(ExtentError::TooManyHolds));
    assert_eq!(ex.holds(e), Some(u16::MAX));
}

#[test]
fn an_extent_of_another_range_is_stale() {
    let mut a = Extents::new(BufId::new(0), 2);
    let mut b = Extents::new(BufId::new(8), 2);
    let from_b = b.take().unwrap();
    assert_eq!(a.hold(from_b), Err(ExtentError::Stale));
    assert_eq!(a.release(from_b), Err(ExtentError::Stale));
}

#[test]
fn a_sync_behind_a_burst_of_receives_is_dispatched_first() {
    // Storage tokens 1000..2000, network tokens 0..1000.
    let turn = Turn::new((1000, 2000), (0, 1000));
    let mut batch = Completions::new(256);
    for i in 0..200 {
        batch.push(Completion::new(Token::new(i), 64));
    }
    batch.push(Completion::new(Token::new(5000), 0)); // a timer
    batch.push(Completion::new(Token::new(1001), 0)); // the write
    batch.push(Completion::new(Token::new(1002), 0)); // the sync
    let mut order = [(Class::Other, 0u64); 203];
    let mut n = 0;
    sealed(|| {
        turn.dispatch(&mut batch, |class, c| {
            order[n] = (class, c.token().get());
            n += 1;
        });
    });
    assert!(batch.is_empty());
    assert_eq!(n, 203);
    assert_eq!(order[0], (Class::Storage, 1001));
    assert_eq!(order[1], (Class::Storage, 1002));
    // The receives follow in arrival order, then the timer.
    for (i, got) in order[2..202].iter().enumerate() {
        assert_eq!(*got, (Class::Network, i as u64));
    }
    assert_eq!(order[202], (Class::Other, 5000));
}

#[test]
fn drain_where_keeps_the_rest_in_order() {
    let mut batch = Completions::new(8);
    for t in [3u64, 10, 4, 11, 5] {
        batch.push(Completion::new(Token::new(t), 0));
    }
    let mut taken = Vec::new();
    batch.drain_where(|c| c.token().get() >= 10, |c| taken.push(c.token().get()));
    assert_eq!(taken, [10, 11]);
    let rest: Vec<u64> = std::iter::from_fn(|| batch.pop())
        .map(|c| c.token().get())
        .collect();
    assert_eq!(rest, [3, 4, 5]);
}
