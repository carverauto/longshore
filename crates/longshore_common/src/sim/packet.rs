// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A simulated link that carries packets between two ends, with seeded
//! loss, duplication, delay and reordering: what a transport with a TCP
//! of its own is tested on, where [`SimIo`](super::SimIo) stands in for
//! the kernel's sockets.
//!
//! An end hands the link whole packets ([`PacketLink::send`]) and takes
//! whole packets ([`PacketLink::recv`]); [`PacketLink::turn`] moves them
//! on at the time the caller gives. The packet memory and the queues are
//! built once and nothing allocates afterwards. The same seed and the
//! same calls give the same packets, in the same order, at the same
//! times.
//!
//! A packet's slot is in exactly one place: free; queued at its sender;
//! in flight, with the time it is due; or delivered, in its receiver's
//! queue. A packet that is lost, or that finds its receiver's queue full,
//! is dropped: counted, and its slot free at once.

mod link;

pub use link::PacketLink;

/// One of the two ends of a link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// The one end.
    A,
    /// The other.
    B,
}

impl End {
    /// The end at the other side.
    pub const fn other(self) -> End {
        match self {
            End::A => End::B,
            End::B => End::A,
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            End::A => 0,
            End::B => 1,
        }
    }
}

/// Why a packet was not taken by [`PacketLink::send`]. Nothing changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketError {
    /// No slot is free, or the sender's queue is full: try after the
    /// next [`PacketLink::turn`], or once the other end has received.
    Full,
    /// Longer than the longest packet the link was built for.
    TooLong,
}

/// How often a packet meets each fault, in parts per million, and how
/// long a delay can be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PacketFaults {
    loss: u32,
    duplicate: u32,
    reorder: u32,
    delay: u32,
    delay_max_ns: u64,
}

impl PacketFaults {
    /// No faults: every packet arrives once, in the order sent.
    pub const NONE: PacketFaults = PacketFaults {
        loss: 0,
        duplicate: 0,
        reorder: 0,
        delay: 0,
        delay_max_ns: 0,
    };

    /// A packet is lost.
    #[must_use]
    pub const fn with_loss(mut self, per_million: u32) -> PacketFaults {
        self.loss = per_million;
        self
    }

    /// A packet arrives twice.
    #[must_use]
    pub const fn with_duplicate(mut self, per_million: u32) -> PacketFaults {
        self.duplicate = per_million;
        self
    }

    /// A packet is held back for [`PacketSizes::with_reorder_ns`], so that
    /// packets sent after it arrive before it.
    #[must_use]
    pub const fn with_reorder(mut self, per_million: u32) -> PacketFaults {
        self.reorder = per_million;
        self
    }

    /// A packet is late by a time drawn up to `max_ns`.
    #[must_use]
    pub const fn with_delay(mut self, per_million: u32, max_ns: u64) -> PacketFaults {
        self.delay = per_million;
        self.delay_max_ns = max_ns;
        self
    }

    pub(super) const fn rates(self) -> (u32, u32, u32, u32, u64) {
        (
            self.loss,
            self.duplicate,
            self.reorder,
            self.delay,
            self.delay_max_ns,
        )
    }
}

/// What a link is built for: how many packets it holds, how long one can
/// be, and its times.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PacketSizes {
    slots: u32,
    queue: u32,
    packet_max: u32,
    latency_ns: u64,
    reorder_ns: u64,
}

impl PacketSizes {
    /// `slots` packets in the link at once, both directions together,
    /// each of up to `packet_max` bytes. An end's transmit queue and its
    /// receive queue hold `slots` packets each, the latency is 100
    /// microseconds, and a reordered packet is held back a millisecond.
    pub const fn new(slots: u32, packet_max: u32) -> PacketSizes {
        PacketSizes {
            slots,
            queue: slots,
            packet_max,
            latency_ns: 100_000,
            reorder_ns: 1_000_000,
        }
    }

    /// Packets an end's transmit queue holds, and its receive queue.
    #[must_use]
    pub const fn with_queue(mut self, packets: u32) -> PacketSizes {
        self.queue = packets;
        self
    }

    /// The time from the turn that sends a packet to its arrival.
    #[must_use]
    pub const fn with_latency_ns(mut self, ns: u64) -> PacketSizes {
        self.latency_ns = ns;
        self
    }

    /// How long a reordered packet is held back.
    #[must_use]
    pub const fn with_reorder_ns(mut self, ns: u64) -> PacketSizes {
        self.reorder_ns = ns;
        self
    }

    pub(super) const fn parts(self) -> (u32, u32, u32, u64, u64) {
        (
            self.slots,
            self.queue,
            self.packet_max,
            self.latency_ns,
            self.reorder_ns,
        )
    }
}

/// What a link has done so far, and where its slots are now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PacketCounts {
    pub(super) sent: u64,
    pub(super) duplicated: u64,
    pub(super) lost: u64,
    pub(super) overrun: u64,
    pub(super) received: u64,
    pub(super) held_back: u64,
    pub(super) delayed: u64,
    pub(super) free: u32,
    pub(super) queued: u32,
    pub(super) in_flight: u32,
    pub(super) delivered: u32,
}

impl PacketCounts {
    /// Packets [`PacketLink::send`] took.
    pub const fn sent(&self) -> u64 {
        self.sent
    }

    /// Duplicates made.
    pub const fn duplicated(&self) -> u64 {
        self.duplicated
    }

    /// Packets lost on the way.
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// Packets dropped because their receiver's queue was full.
    pub const fn overrun(&self) -> u64 {
        self.overrun
    }

    /// Packets [`PacketLink::recv`] handed out.
    pub const fn received(&self) -> u64 {
        self.received
    }

    /// Packets held back to arrive out of order.
    pub const fn held_back(&self) -> u64 {
        self.held_back
    }

    /// Packets that were late by a drawn time.
    pub const fn delayed(&self) -> u64 {
        self.delayed
    }

    /// Slots free, queued at a sender, in flight, and delivered but not
    /// yet received: together, every slot of the link.
    pub const fn places(&self) -> [u32; 4] {
        [self.free, self.queued, self.in_flight, self.delivered]
    }
}
