// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The link itself: its slots, its queues, and what each call does.

use super::{End, PacketCounts, PacketError, PacketFaults, PacketSizes};
use crate::io::trace;
use crate::sim::prng::Prng;

/// A queue of slots with room for a fixed number of them.
struct Ring {
    slots: Box<[u32]>,
    head: usize,
    len: usize,
}

impl Ring {
    fn new(room: u32) -> Ring {
        Ring {
            slots: vec![0; room as usize].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    const fn is_full(&self) -> bool {
        self.len == self.slots.len()
    }

    fn push(&mut self, slot: u32) {
        let at = (self.head + self.len) % self.slots.len();
        self.slots[at] = slot;
        self.len += 1;
    }

    fn front(&self) -> Option<u32> {
        (self.len > 0).then(|| self.slots[self.head])
    }

    fn pop(&mut self) -> Option<u32> {
        if self.len == 0 {
            return None;
        }
        let slot = self.slots[self.head];
        self.head = (self.head + 1) % self.slots.len();
        self.len -= 1;
        Some(slot)
    }
}

/// What is known of the packet in a slot.
#[derive(Clone, Copy)]
struct Packet {
    len: u32,
    /// The end it is for.
    to: End,
    /// Which packet of the link it is, by the order `send` took them; a
    /// duplicate has its original's.
    number: u64,
    /// When it is due, and its place among those due at the same time.
    due: u64,
    order: u64,
}

/// A link between two ends. See the [module](super) for what it does.
pub struct PacketLink {
    faults: PacketFaults,
    sizes: PacketSizes,
    rng: Prng,
    bytes: Box<[u8]>,
    packets: Box<[Packet]>,
    free: Box<[u32]>,
    free_len: usize,
    /// Each end's transmit queue, and its receive queue.
    tx: [Ring; 2],
    rx: [Ring; 2],
    /// The slots in flight, as a heap: the one due first is at the front.
    flight: Box<[u32]>,
    flight_len: usize,
    next_order: u64,
    next_number: u64,
    counts: PacketCounts,
    trace: u64,
}

impl PacketLink {
    /// A link of `sizes` whose faults come from `seed`. Allocates its
    /// memory once, here.
    pub fn new(seed: u64, faults: PacketFaults, sizes: PacketSizes) -> PacketLink {
        let (slots, queue, packet_max) = (sizes.slots, sizes.queue, sizes.packet_max);
        let idle = Packet {
            len: 0,
            to: End::A,
            number: 0,
            due: 0,
            order: 0,
        };
        PacketLink {
            faults,
            sizes,
            rng: Prng::new(seed),
            bytes: vec![0; slots as usize * packet_max as usize].into_boxed_slice(),
            packets: vec![idle; slots as usize].into_boxed_slice(),
            free: (0..slots).rev().collect(),
            free_len: slots as usize,
            tx: [Ring::new(queue), Ring::new(queue)],
            rx: [Ring::new(queue), Ring::new(queue)],
            flight: vec![0; slots as usize].into_boxed_slice(),
            flight_len: 0,
            next_order: 0,
            next_number: 0,
            counts: PacketCounts::default(),
            trace: trace::START,
        }
    }

    /// The bytes of `slot`'s memory.
    fn memory(&mut self, slot: u32) -> &mut [u8] {
        let size = self.sizes.packet_max as usize;
        let at = slot as usize * size;
        &mut self.bytes[at..at + size]
    }

    fn take_slot(&mut self) -> Option<u32> {
        let top = self.free_len.checked_sub(1)?;
        self.free_len = top;
        Some(self.free[top])
    }

    fn free_slot(&mut self, slot: u32) {
        self.free[self.free_len] = slot;
        self.free_len += 1;
    }

    /// Fold an event into the trace.
    fn note(&mut self, what: u64, slot: u32, at: u64) {
        let p = self.packets[slot as usize];
        let parts = [what, p.number, u64::from(p.len), p.to.index() as u64, at];
        trace::fold(&mut self.trace, &parts);
    }

    /// Queue `bytes` as a packet from `from` to the other end.
    pub fn send(&mut self, from: End, bytes: &[u8]) -> Result<(), PacketError> {
        if bytes.len() > self.sizes.packet_max as usize {
            return Err(PacketError::TooLong);
        }
        if self.tx[from.index()].is_full() {
            return Err(PacketError::Full);
        }
        let slot = self.take_slot().ok_or(PacketError::Full)?;
        self.memory(slot)[..bytes.len()].copy_from_slice(bytes);
        let packet = &mut self.packets[slot as usize];
        (packet.len, packet.to) = (bytes.len() as u32, from.other());
        packet.number = self.next_number;
        self.next_number += 1;
        self.tx[from.index()].push(slot);
        self.counts.sent += 1;
        Ok(())
    }

    /// Put `slot` in flight from a turn at `now`, with its own draws of
    /// delay and reordering.
    fn launch(&mut self, slot: u32, now: u64) {
        let mut due = now.saturating_add(self.sizes.latency_ns);
        if self.rng.chance(self.faults.delay) {
            let longest = self.faults.delay_max_ns.saturating_add(1);
            due = due.saturating_add(self.rng.below(longest));
            self.counts.delayed += 1;
        }
        if self.rng.chance(self.faults.reorder) {
            due = due.saturating_add(self.sizes.reorder_ns);
            self.counts.held_back += 1;
        }
        let packet = &mut self.packets[slot as usize];
        (packet.due, packet.order) = (due, self.next_order);
        self.next_order += 1;
        self.flight_push(slot);
    }

    /// When `slot` is due, and its place among those due at that time.
    fn key(&self, slot: u32) -> (u64, u64) {
        let p = &self.packets[slot as usize];
        (p.due, p.order)
    }

    /// Add `slot` to those in flight.
    fn flight_push(&mut self, slot: u32) {
        let mut at = self.flight_len;
        self.flight[at] = slot;
        self.flight_len += 1;
        while at > 0 {
            let parent = (at - 1) / 2;
            if self.key(self.flight[parent]) <= self.key(self.flight[at]) {
                break;
            }
            self.flight.swap(parent, at);
            at = parent;
        }
    }

    /// Take the slot due first from those in flight, if it is due at
    /// `now`.
    fn flight_pop(&mut self, now: u64) -> Option<u32> {
        let first = *self.flight[..self.flight_len].first()?;
        if self.key(first).0 > now {
            return None;
        }
        self.flight_len -= 1;
        self.flight[0] = self.flight[self.flight_len];
        let mut at = 0;
        loop {
            let (left, right) = (2 * at + 1, 2 * at + 2);
            let mut least = at;
            for child in [left, right] {
                let nearer = child < self.flight_len
                    && self.key(self.flight[child]) < self.key(self.flight[least]);
                if nearer {
                    least = child;
                }
            }
            if least == at {
                return Some(first);
            }
            self.flight.swap(least, at);
            at = least;
        }
    }

    /// Send what `from` has queued: each packet is lost, or in flight,
    /// and then perhaps twice.
    fn depart(&mut self, from: End, now: u64) {
        while let Some(slot) = self.tx[from.index()].pop() {
            if self.rng.chance(self.faults.loss) {
                self.counts.lost += 1;
                self.note(1, slot, now);
                self.free_slot(slot);
                continue;
            }
            self.launch(slot, now);
            if !self.rng.chance(self.faults.duplicate) {
                continue;
            }
            // A duplicate that finds no slot is not made.
            let Some(twin) = self.take_slot() else {
                continue;
            };
            let size = self.sizes.packet_max as usize;
            let len = self.packets[slot as usize].len as usize;
            let (from_at, to_at) = (slot as usize * size, twin as usize * size);
            self.bytes.copy_within(from_at..from_at + len, to_at);
            self.packets[twin as usize] = self.packets[slot as usize];
            self.launch(twin, now);
            self.counts.duplicated += 1;
        }
    }

    /// Move packets on at time `now`: what is queued departs, and what is
    /// due arrives in its receiver's queue, or is dropped if that is full.
    pub fn turn(&mut self, now: u64) {
        self.depart(End::A, now);
        self.depart(End::B, now);
        while let Some(slot) = self.flight_pop(now) {
            let to = self.packets[slot as usize].to.index();
            if self.rx[to].is_full() {
                self.counts.overrun += 1;
                self.note(2, slot, now);
                self.free_slot(slot);
            } else {
                self.note(3, slot, now);
                self.rx[to].push(slot);
            }
        }
    }

    /// The length of the oldest packet delivered to `at`, which the next
    /// [`PacketLink::recv`] there hands out.
    pub fn next_len(&self, at: End) -> Option<usize> {
        let slot = self.rx[at.index()].front()?;
        Some(self.packets[slot as usize].len as usize)
    }

    /// The oldest packet delivered to `at`, copied into `into`: its
    /// length. `None` when there is none, and when `into` is shorter than
    /// the packet: it stays then, whole, and [`PacketLink::next_len`]
    /// tells how long it is.
    pub fn recv(&mut self, at: End, into: &mut [u8]) -> Option<usize> {
        let len = self.next_len(at).filter(|len| *len <= into.len())?;
        let slot = self.rx[at.index()].pop()?;
        into[..len].copy_from_slice(&self.memory(slot)[..len]);
        self.free_slot(slot);
        self.counts.received += 1;
        Some(len)
    }

    /// The earliest time at which [`PacketLink::turn`] has something to
    /// do: `now` when a packet is queued, else when the next one in
    /// flight is due. `None` when nothing is on its way.
    pub fn next_due(&self, now: u64) -> Option<u64> {
        if self.tx[0].len + self.tx[1].len > 0 {
            return Some(now);
        }
        // Never earlier than `now`: a packet whose time has passed is due
        // at once.
        let first = self.flight[..self.flight_len].first()?;
        Some(self.key(*first).0.max(now))
    }

    /// What the link has done, and where its slots are.
    pub fn counts(&self) -> PacketCounts {
        let mut counts = self.counts;
        counts.free = self.free_len as u32;
        counts.queued = (self.tx[0].len + self.tx[1].len) as u32;
        counts.in_flight = self.flight_len as u32;
        counts.delivered = (self.rx[0].len + self.rx[1].len) as u32;
        counts
    }

    /// A hash of every packet lost, dropped and delivered so far, with
    /// its time: the same for the same seed and the same calls.
    pub const fn trace_hash(&self) -> u64 {
        self.trace
    }
}
