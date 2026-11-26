// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The receive area of [`Virtual`]: pages a zero-copy receive delivers
//! pieces of, as a kernel does for a card that writes into them.
//!
//! A page has a count of the pieces out of it, and is not written again
//! before every one is back. A piece is known by its page, its offset
//! and its length: one returned that is not out, or returned twice,
//! stops the run, and its bytes are overwritten when it goes back, so
//! that a read after the return is seen.

use super::*;

/// What a returned piece's bytes are overwritten with.
pub(in crate::sim) const RETURNED: u8 = 0xDD;

/// A piece out: bytes `off..off + len` of page `page`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::sim) struct Piece {
    page: u32,
    off: u32,
    len: u32,
}

/// The pages of the area and the pieces out of them.
pub(in crate::sim) struct Area {
    /// The first buffer id of the area's pages.
    base: u32,
    /// Pieces out of each page.
    refs: Box<[u16]>,
    /// The socket each page is being filled for, plus one; 0 for none.
    filling: Box<[u32]>,
    /// Pages free to be filled, a stack.
    free: Box<[u32]>,
    free_len: usize,
    /// Every piece out, in no order.
    out: Box<[Piece]>,
    out_len: usize,
    /// For each socket, the page it fills and how far, if any.
    at: Box<[Option<(u32, u32)>]>,
}

impl Area {
    pub(super) fn new(limits: &Limits) -> Area {
        let pages = limits.area_pages as usize;
        Area {
            base: limits.area_base(),
            refs: vec![0; pages].into_boxed_slice(),
            filling: vec![0; pages].into_boxed_slice(),
            free: (0..pages as u32).rev().collect(),
            free_len: pages,
            // A page backs at most a piece a byte.
            out: vec![
                Piece {
                    page: 0,
                    off: 0,
                    len: 0
                };
                pages * Limits::PAGE as usize
            ]
            .into_boxed_slice(),
            out_len: 0,
            at: vec![None; limits.sockets_max as usize].into_boxed_slice(),
        }
    }

    /// As when built: every page free, no piece out.
    pub(super) fn reset(&mut self) {
        let pages = self.refs.len();
        self.refs.fill(0);
        self.filling.fill(0);
        for (i, slot) in self.free.iter_mut().enumerate() {
            *slot = (pages - 1 - i) as u32;
        }
        (self.free_len, self.out_len) = (pages, 0);
        self.at.fill(None);
    }

    /// Pieces out, and pages with a piece out: the simulator's own count,
    /// for a check that does not trust the code under test.
    pub(super) fn out(&self) -> (usize, usize) {
        (self.out_len, self.refs.iter().filter(|n| **n > 0).count())
    }

    /// A page with no piece out and no socket filling it is free again.
    fn settle(&mut self, page: u32) {
        if self.refs[page as usize] == 0 && self.filling[page as usize] == 0 {
            self.free[self.free_len] = page;
            self.free_len += 1;
        }
    }

    /// `sock` fills no page any more: its connection is over.
    pub(super) fn leave(&mut self, sock: u32) {
        if let Some((page, _)) = self.at[sock as usize].take() {
            self.filling[page as usize] = 0;
            self.settle(page);
        }
    }

    /// Where the next piece of `sock` goes: its page from where it is
    /// filled to, or a new page when `fresh` or that one is full. `None`
    /// when no page is free.
    fn place(&mut self, sock: u32, fresh: bool) -> Option<(u32, u32)> {
        if let Some((page, filled)) = self.at[sock as usize] {
            if !fresh && filled < Limits::PAGE {
                return Some((page, filled));
            }
            self.leave(sock);
        }
        let top = self.free_len.checked_sub(1)?;
        let page = self.free[top];
        self.free_len = top;
        self.filling[page as usize] = sock + 1;
        self.at[sock as usize] = Some((page, 0));
        Some((page, 0))
    }

    /// Note a piece of `len` bytes delivered to `sock` at `off` of `page`.
    fn deliver(&mut self, sock: u32, page: u32, off: u32, len: u32) {
        self.refs[page as usize] += 1;
        self.out[self.out_len] = Piece { page, off, len };
        self.out_len += 1;
        self.at[sock as usize] = Some((page, off + len));
    }

    /// The piece `len` bytes of `id` from `off` is back: its page index.
    /// Stops the run for a piece that is not out.
    fn back(&mut self, id: BufId, off: u32, len: u32) -> u32 {
        let page = id.0 - self.base;
        let piece = Piece { page, off, len };
        let found = self.out[..self.out_len].iter().position(|p| *p == piece);
        assert!(
            found.is_some(),
            "a piece returned that is not out: {len} bytes of page {page} from {off}"
        );
        let Some(at) = found else {
            return page;
        };
        self.out_len -= 1;
        self.out[at] = self.out[self.out_len];
        self.refs[page as usize] -= 1;
        self.settle(page);
        page
    }
}

impl Virtual {
    /// One piece for an armed zero-copy receive of `sock` with `credit`
    /// bytes left: seeded in length, in the page the socket fills or,
    /// seeded, a new one, as a new packet starts one.
    pub(super) fn attempt_recv_zc(
        &mut self,
        sock: Sock,
        credit: u32,
        pool: &mut [u8],
        limits: &Limits,
    ) -> Option<Attempt> {
        let s = sock.0 as usize;
        if self.socks[s].reset {
            return Some(Attempt::single(-ECONNRESET));
        }
        if self.socks[s].state != SockState::Open {
            return Some(Attempt::single(-EPIPE));
        }
        if self.socks[s].rd_shut {
            return Some(Attempt::single(0));
        }
        let avail = self.socks[s].rx_len;
        if avail == 0 {
            if self.socks[s].peer_closed || self.socks[s].peer_wr_shut {
                return Some(Attempt::single(0));
            }
            return None;
        }
        if self.rng.chance(self.faults.reset) {
            self.reset_pair(sock.0);
            return Some(Attempt::single(-ECONNRESET));
        }
        // One packet in four starts a page, whatever room the last left.
        let fresh = self.rng.below(4) == 0;
        let (page, off) = self.area.place(sock.0, fresh)?;
        let mut n = (Limits::PAGE - off).min(credit).min(avail as u32);
        if self.rng.chance(self.faults.partial_recv) {
            n = 1 + self.rng.below(u64::from(n)) as u32;
        }
        let id = BufId(limits.area_base() + page);
        let dst = limits.range(id, off, n);
        let q = &mut self.socks[s];
        for k in 0..n as usize {
            pool[dst.start + k] = q.rx[(q.rx_head + k) % SOCK_BUF];
        }
        q.rx_head = (q.rx_head + n as usize) % SOCK_BUF;
        q.rx_len -= n as usize;
        self.area.deliver(sock.0, page, off, n);
        Some(Attempt::piece(count_result(n as usize), id, off))
    }

    /// A piece is back: checked against what is out, and overwritten.
    pub(super) fn give_back_piece_of(
        &mut self,
        id: BufId,
        off: u32,
        len: u32,
        pool: &mut [u8],
        limits: &Limits,
    ) {
        let _page: u32 = self.area.back(id, off, len);
        pool[limits.range(id, off, len)].fill(RETURNED);
    }
}
