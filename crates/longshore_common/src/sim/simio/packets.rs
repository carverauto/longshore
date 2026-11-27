// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where the packets of a stream begin, for [`Virtual`].
//!
//! Every send on a stream socket is a packet: its first byte is noted in
//! the receiving socket, by how many bytes were ever queued in that
//! socket before it. A count that only grows never comes round to a
//! place that was taken already, as a place in the socket's ring would.
//! A receive into the receive area cuts its pieces from packets: a packet
//! starts a page, as a card lays each packet's payload at the start of
//! one, and no piece runs from one packet into the next. The lengths of
//! the last packets sent are kept as well, for a test that asks what a
//! sender put into one send.

use super::*;

/// Packets whose start a socket can have ahead of it; more are joined
/// to the one before.
const MARKS: usize = 32;
/// Lengths of the last packets that are kept.
const LOG: usize = 256;

/// Where the packets waiting in each socket begin, and how long the last
/// packets of all sockets were.
pub(in crate::sim) struct Packets {
    /// For each socket, the bytes ever queued in it before each waiting
    /// packet: a ring of `MARKS`, oldest at `head`.
    marks: Box<[u64]>,
    head: Box<[u8]>,
    len: Box<[u8]>,
    /// For each socket, the bytes ever queued in it.
    queued: Box<[u64]>,
    /// The lengths of the last packets sent, a ring, and how many were
    /// ever sent.
    lens: Box<[u32]>,
    sent: u64,
    /// For each socket, whether its sends each end a buffer: a packet that
    /// follows one is never joined to it.
    ends: Box<[bool]>,
}

/// The bit of a mark that says the packet before it ended a buffer.
const FIRM: u64 = 1 << 63;

impl Packets {
    pub(super) fn new(limits: &Limits) -> Packets {
        let socks = limits.sockets_max as usize;
        Packets {
            marks: vec![0; socks * MARKS].into_boxed_slice(),
            head: vec![0; socks].into_boxed_slice(),
            len: vec![0; socks].into_boxed_slice(),
            queued: vec![0; socks].into_boxed_slice(),
            lens: vec![0; LOG].into_boxed_slice(),
            sent: 0,
            ends: vec![false; socks].into_boxed_slice(),
        }
    }

    /// As when built.
    pub(super) fn reset(&mut self) {
        self.head.fill(0);
        self.len.fill(0);
        self.queued.fill(0);
        self.sent = 0;
        self.ends.fill(false);
    }

    /// Every send of `sock` from now on ends a buffer, or not.
    pub(super) fn end_sends(&mut self, sock: u32, on: bool) {
        self.ends[sock as usize] = on;
    }

    /// No packet waits in `sock` any more, and its sends end no buffer:
    /// its connection is over, or a new one begins.
    pub(super) fn forget(&mut self, sock: u32) {
        (self.head[sock as usize], self.len[sock as usize]) = (0, 0);
        self.ends[sock as usize] = false;
    }

    /// A packet of `n` bytes from `from` was queued in `sock`, behind the
    /// `waiting` bytes that were in its queue before.
    pub(super) fn sent(&mut self, (from, sock): (u32, u32), waiting: usize, n: usize) {
        let firm = if self.ends[from as usize] { FIRM } else { 0 };
        self.lens[(self.sent % LOG as u64) as usize] = n as u32;
        self.sent += 1;
        let s = sock as usize;
        let at = self.queued[s];
        self.queued[s] += n as u64;
        // Marks of bytes that were taken already make no room for others.
        self.behind(s, at.saturating_sub(waiting as u64));
        if (self.len[s] as usize) < MARKS {
            let slot = (self.head[s] as usize + self.len[s] as usize) % MARKS;
            self.marks[s * MARKS + slot] = at | firm;
            self.len[s] += 1;
        }
    }

    /// The `k`-th waiting mark of `sock`: the bytes ever queued before it.
    fn mark(&self, sock: usize, k: usize) -> u64 {
        self.marks[sock * MARKS + (self.head[sock] as usize + k) % MARKS] & !FIRM
    }

    /// Drops the marks of `sock` before `read`, the bytes ever taken.
    fn behind(&mut self, s: usize, read: u64) {
        while self.len[s] > 0 && self.mark(s, 0) < read {
            self.head[s] = ((self.head[s] as usize + 1) % MARKS) as u8;
            self.len[s] -= 1;
        }
    }

    /// What lies ahead of a socket that has `rx_len` bytes waiting:
    /// whether a packet begins with the next byte, whether it follows a
    /// send that ended a buffer (and so cannot have been joined to the one
    /// before), and how many bytes come before the next packet begins.
    pub(super) fn ahead(&mut self, sock: u32, rx_len: usize) -> (bool, bool, usize) {
        let s = sock as usize;
        // What waits are the last bytes queued: the rest were taken, or
        // thrown away with the queue.
        let read = self.queued[s].saturating_sub(rx_len as u64);
        self.behind(s, read);
        if self.len[s] == 0 {
            return (false, false, rx_len);
        }
        let first = (self.mark(s, 0) - read) as usize;
        if first > 0 {
            return (false, false, first.min(rx_len));
        }
        let firm = self.marks[s * MARKS + self.head[s] as usize] & FIRM != 0;
        let next = if self.len[s] > 1 {
            ((self.mark(s, 1) - read) as usize).min(rx_len)
        } else {
            rx_len
        };
        // Two marks at one place cannot be: a packet has a byte.
        (true, firm, next.max(1))
    }

    /// The lengths of the last packets sent, oldest first, into `into`:
    /// how many, and how many were ever sent.
    pub(super) fn last(&self, into: &mut [u32]) -> (usize, u64) {
        let have = (self.sent.min(LOG as u64) as usize).min(into.len());
        for (i, slot) in into[..have].iter_mut().enumerate() {
            let at = self.sent - have as u64 + i as u64;
            *slot = self.lens[(at % LOG as u64) as usize];
        }
        (have, self.sent)
    }
}
