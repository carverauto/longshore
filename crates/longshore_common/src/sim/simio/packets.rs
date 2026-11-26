// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where the packets of a stream begin, for [`Virtual`].
//!
//! Every send on a stream socket is a packet: its first byte is noted in
//! the receiving socket, by where it lies in that socket's queue. A
//! receive into the receive area cuts its pieces from packets: a packet
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
    /// For each socket, the places in its queue where a packet begins:
    /// a ring of `MARKS`, oldest at `head`.
    marks: Box<[u32]>,
    head: Box<[u8]>,
    len: Box<[u8]>,
    /// The lengths of the last packets sent, a ring, and how many were
    /// ever sent.
    lens: Box<[u32]>,
    sent: u64,
}

impl Packets {
    pub(super) fn new(limits: &Limits) -> Packets {
        let socks = limits.sockets_max as usize;
        Packets {
            marks: vec![0; socks * MARKS].into_boxed_slice(),
            head: vec![0; socks].into_boxed_slice(),
            len: vec![0; socks].into_boxed_slice(),
            lens: vec![0; LOG].into_boxed_slice(),
            sent: 0,
        }
    }

    /// As when built.
    pub(super) fn reset(&mut self) {
        self.head.fill(0);
        self.len.fill(0);
        self.sent = 0;
    }

    /// No packet waits in `sock` any more: its connection is over.
    pub(super) fn forget(&mut self, sock: u32) {
        (self.head[sock as usize], self.len[sock as usize]) = (0, 0);
    }

    /// A packet of `n` bytes was queued in `sock`, its first byte at `at`
    /// of the socket's queue.
    pub(super) fn sent(&mut self, sock: u32, at: usize, n: usize) {
        self.lens[(self.sent % LOG as u64) as usize] = n as u32;
        self.sent += 1;
        let s = sock as usize;
        if (self.len[s] as usize) < MARKS {
            let slot = (self.head[s] as usize + self.len[s] as usize) % MARKS;
            self.marks[s * MARKS + slot] = at as u32;
            self.len[s] += 1;
        }
    }

    /// The `k`-th waiting mark of `sock`, as bytes ahead of `rx_head`.
    fn ahead_of(&self, sock: usize, k: usize, rx_head: usize) -> usize {
        let mark = self.marks[sock * MARKS + (self.head[sock] as usize + k) % MARKS] as usize;
        (mark + SOCK_BUF - rx_head) % SOCK_BUF
    }

    /// What lies ahead of a socket that has `rx_len` bytes waiting from
    /// `rx_head`: whether a packet begins with the next byte, and how many
    /// bytes come before the next packet begins.
    pub(super) fn ahead(&mut self, sock: u32, rx_head: usize, rx_len: usize) -> (bool, usize) {
        let s = sock as usize;
        // Marks of bytes that were taken already are behind.
        while self.len[s] > 0 && self.ahead_of(s, 0, rx_head) >= rx_len {
            self.head[s] = ((self.head[s] as usize + 1) % MARKS) as u8;
            self.len[s] -= 1;
        }
        if self.len[s] == 0 {
            return (false, rx_len);
        }
        let first = self.ahead_of(s, 0, rx_head);
        if first > 0 {
            return (false, first);
        }
        let next = if self.len[s] > 1 {
            self.ahead_of(s, 1, rx_head).min(rx_len)
        } else {
            rx_len
        };
        // Two marks at one place cannot be: a packet has a byte.
        (true, next.max(1))
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
