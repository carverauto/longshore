// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters and `with_*` setters for [`Faults`].

use super::Faults;

impl Faults {
    /// No faults: what `Faults::default()` gives, usable in `const` items.
    pub const NONE: Self = Self {
        connect_refused: 0,
        reset: 0,
        short_send: 0,
        partial_recv: 0,
        bit_flip: 0,
        delay: 0,
        udp_drop: 0,
        reorder: 0,
    };

    /// The reports of a closed socket's operations come in another order.
    ///
    /// A close ends every operation in flight on its socket, and a real
    /// kernel reports those ends and the close's own in no fixed order,
    /// some of them long after. Without this the simulator reports the
    /// close first and the others within a step or two. With it, per
    /// step: an operation that a close ended keeps its report for a later
    /// step, and a close that is about to complete ends its socket's
    /// operations first and completes in a later step itself. An
    /// operation cancelled by name (`Op::Cancel`) is not affected. Taken
    /// to be at most 900,000, so that every report does come.
    #[inline]
    pub const fn reorder(self) -> u32 {
        self.reorder
    }

    /// This value with [`Faults::reorder`] set to `reorder`.
    #[inline]
    pub const fn with_reorder(mut self, reorder: u32) -> Self {
        self.reorder = reorder;
        self
    }

    /// A connect is refused.
    #[inline]
    pub const fn connect_refused(self) -> u32 {
        self.connect_refused
    }

    /// This value with [`Faults::connect_refused`] set to `connect_refused`.
    #[inline]
    pub const fn with_connect_refused(mut self, connect_refused: u32) -> Self {
        self.connect_refused = connect_refused;
        self
    }

    /// A send or receive resets the connection.
    #[inline]
    pub const fn reset(self) -> u32 {
        self.reset
    }

    /// This value with [`Faults::reset`] set to `reset`.
    #[inline]
    pub const fn with_reset(mut self, reset: u32) -> Self {
        self.reset = reset;
        self
    }

    /// A send accepts fewer bytes than asked.
    #[inline]
    pub const fn short_send(self) -> u32 {
        self.short_send
    }

    /// This value with [`Faults::short_send`] set to `short_send`.
    #[inline]
    pub const fn with_short_send(mut self, short_send: u32) -> Self {
        self.short_send = short_send;
        self
    }

    /// A receive returns fewer bytes than are available.
    #[inline]
    pub const fn partial_recv(self) -> u32 {
        self.partial_recv
    }

    /// This value with [`Faults::partial_recv`] set to `partial_recv`.
    #[inline]
    pub const fn with_partial_recv(mut self, partial_recv: u32) -> Self {
        self.partial_recv = partial_recv;
        self
    }

    /// One bit of a sent chunk is flipped in transit.
    #[inline]
    pub const fn bit_flip(self) -> u32 {
        self.bit_flip
    }

    /// This value with [`Faults::bit_flip`] set to `bit_flip`.
    #[inline]
    pub const fn with_bit_flip(mut self, bit_flip: u32) -> Self {
        self.bit_flip = bit_flip;
        self
    }

    /// A ready operation is held back for this tick (delays and reorders).
    #[inline]
    pub const fn delay(self) -> u32 {
        self.delay
    }

    /// This value with [`Faults::delay`] set to `delay`.
    #[inline]
    pub const fn with_delay(mut self, delay: u32) -> Self {
        self.delay = delay;
        self
    }

    /// A UDP datagram is lost. Corrupted datagrams are also lost, as the UDP
    /// checksum would discard them.
    #[inline]
    pub const fn udp_drop(self) -> u32 {
        self.udp_drop
    }

    /// This value with [`Faults::udp_drop`] set to `udp_drop`.
    #[inline]
    pub const fn with_udp_drop(mut self, udp_drop: u32) -> Self {
        self.udp_drop = udp_drop;
        self
    }
}
