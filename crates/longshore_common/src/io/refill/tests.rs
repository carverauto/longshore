// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The ring of returns against memory that stands in for the kernel's: it
//! is never written past what it holds, what does not fit waits and goes
//! in when the kernel has read, and a flush is asked for exactly then.

use super::{Refill, Rqe};
use core::sync::atomic::{AtomicU32, Ordering};

const ENTRIES: u32 = 8;
const TOKEN: u64 = 0xA5 << 48;

/// The kernel's side: its counters and the entries, and every entry it
/// has read.
struct Kernel {
    head: Box<AtomicU32>,
    tail: Box<AtomicU32>,
    rqes: Box<[Rqe]>,
    read: Vec<Rqe>,
}

impl Kernel {
    fn new() -> Kernel {
        Kernel {
            head: Box::new(AtomicU32::new(0)),
            tail: Box::new(AtomicU32::new(0)),
            rqes: vec![Rqe::default(); ENTRIES as usize].into_boxed_slice(),
            read: Vec::new(),
        }
    }

    fn refill(&mut self, waiting: usize) -> Refill {
        let counters = (&raw const *self.head, &raw const *self.tail);
        unsafe { Refill::new(counters, self.rqes.as_mut_ptr(), ENTRIES, waiting, TOKEN) }
    }

    /// Entries written and not read.
    fn pending(&self) -> u32 {
        self.tail.load(Ordering::Acquire) - self.head.load(Ordering::Acquire)
    }

    /// Read up to `most` entries, as the kernel does: never more than the
    /// ring has, whatever the tail says.
    fn take(&mut self, most: u32) {
        let n = self.pending().min(ENTRIES).min(most);
        let head = self.head.load(Ordering::Acquire);
        for k in 0..n {
            self.read.push(self.rqes[((head + k) % ENTRIES) as usize]);
        }
        self.head.store(head + n, Ordering::Release);
    }
}

/// The offsets `from..to`, each a piece of its own page.
fn pages(from: u64, to: u64) -> impl Iterator<Item = u64> {
    (from..to).map(|page| page * 4096)
}

#[test]
fn an_entry_carries_the_offset_under_the_token_and_the_length() {
    let mut kernel = Kernel::new();
    let mut refill = kernel.refill(4);
    refill.give(3 * 4096 + 256, 1000);
    kernel.take(1);
    let wrote = Rqe {
        off: TOKEN | (3 * 4096 + 256),
        len: 1000,
        pad: 0,
    };
    assert_eq!(kernel.read, [wrote]);
    assert!(!refill.wants_flush());
}

#[test]
fn the_ring_never_holds_more_than_it_has_and_nothing_is_returned_twice() {
    let mut kernel = Kernel::new();
    let mut refill = kernel.refill(16);
    // Three rings' worth, with the kernel reading a few now and then.
    for (i, off) in pages(0, 24).enumerate() {
        refill.give(off, 4096);
        assert!(
            kernel.pending() <= ENTRIES,
            "{} in a ring of 8",
            kernel.pending()
        );
        if i % 5 == 4 {
            kernel.take(3);
            refill.drain();
            assert!(kernel.pending() <= ENTRIES);
        }
    }
    while refill.waiting() > 0 || kernel.pending() > 0 {
        kernel.take(ENTRIES);
        refill.flushed();
    }
    let mut back: Vec<u64> = kernel.read.iter().map(|e| e.off & !TOKEN).collect();
    back.sort_unstable();
    assert_eq!(back, pages(0, 24).collect::<Vec<_>>());
    assert!(!refill.wants_flush());
}

#[test]
fn a_return_that_finds_the_ring_full_waits_and_asks_for_a_flush() {
    let mut kernel = Kernel::new();
    let mut refill = kernel.refill(4);
    for off in pages(0, 8) {
        refill.give(off, 4096);
    }
    assert!(!refill.wants_flush(), "a full ring alone asks for nothing");
    refill.give(8 * 4096, 4096);
    assert_eq!((refill.waiting(), kernel.pending()), (1, ENTRIES));
    assert!(refill.wants_flush());
    // A flush that read nothing leaves it waiting, and asked for again.
    refill.flushed();
    assert_eq!(refill.waiting(), 1);
    assert!(refill.wants_flush());
    kernel.take(ENTRIES);
    refill.flushed();
    assert_eq!((refill.waiting(), kernel.pending()), (0, 1));
    assert!(!refill.wants_flush());
}

#[test]
fn past_the_room_it_was_built_with_the_queue_grows_and_loses_nothing() {
    let mut kernel = Kernel::new();
    let mut refill = kernel.refill(2);
    for off in pages(0, 13) {
        refill.give(off, 4096);
    }
    assert_eq!((refill.waiting(), kernel.pending()), (5, ENTRIES));
    while refill.waiting() > 0 || kernel.pending() > 0 {
        kernel.take(ENTRIES);
        refill.flushed();
    }
    assert_eq!(kernel.read.len(), 13);
    assert!(!refill.wants_flush());
}

#[test]
fn a_return_goes_behind_what_waits_though_the_ring_has_room_again() {
    let mut kernel = Kernel::new();
    let mut refill = kernel.refill(4);
    for off in pages(0, 9) {
        refill.give(off, 4096);
    }
    kernel.take(2);
    // Room for two, and one waits: the new one waits behind it.
    refill.give(9 * 4096, 4096);
    assert_eq!(refill.waiting(), 2);
    refill.drain();
    assert_eq!((refill.waiting(), kernel.pending()), (0, ENTRIES));
}

#[test]
fn a_flush_asked_for_is_wanted_until_it_is_done() {
    let mut kernel = Kernel::new();
    let mut refill = kernel.refill(4);
    assert!(!refill.wants_flush());
    refill.ask_flush();
    assert!(refill.wants_flush());
    refill.flushed();
    assert!(!refill.wants_flush());
}
