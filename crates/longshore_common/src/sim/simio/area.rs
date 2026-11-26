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
//!
//! Pages are the card's pool's, or, a quarter of them, the free list a
//! kernel copies into for payload that did not arrive in the area. A
//! page whose last piece is back is not free at once: it waits among the
//! returns until they are read, which is when the pool has nothing left
//! or the returns are as many as fit, and goes back to where it came
//! from then. A pool found empty stays so for a while after.

use super::*;
use crate::io::{ENOMEM, ENOSPC};

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
    /// Whether each page is being filled for a socket.
    filling: Box<[bool]>,
    /// Whether each page is of the copy path's free list.
    copied: Box<[bool]>,
    /// The card's pool, the copy path's free list, and the pages returned
    /// and not yet read back into either: stacks, and how many each holds.
    pool: Box<[u32]>,
    list: Box<[u32]>,
    back: Box<[u32]>,
    pool_len: usize,
    list_len: usize,
    back_len: usize,
    /// Returns that are read back when this many wait.
    back_most: usize,
    /// Every piece out, in no order.
    out: Box<[Piece]>,
    out_len: usize,
    /// For each socket, the page it fills and how far, if any; and
    /// whether its receive was left silent by a full completion ring.
    at: Box<[Option<(u32, u32)>]>,
    silent: Box<[bool]>,
    /// The time until which the card is given no page, and the copy path
    /// none: each found nothing to take.
    starved_until: u64,
    dry_until: u64,
}

impl Area {
    pub(super) fn new(limits: &Limits, seed: u64) -> Area {
        let pages = limits.area_pages as usize;
        let mut area = Area {
            base: limits.area_base(),
            refs: vec![0; pages].into_boxed_slice(),
            filling: vec![false; pages].into_boxed_slice(),
            copied: vec![false; pages].into_boxed_slice(),
            pool: vec![0; pages].into_boxed_slice(),
            list: vec![0; pages].into_boxed_slice(),
            back: vec![0; pages].into_boxed_slice(),
            pool_len: 0,
            list_len: 0,
            back_len: 0,
            // A small number that the seed chooses, as the size of a
            // ring of returns that is read lazily.
            back_most: 1 + (seed % 7) as usize,
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
            silent: vec![false; limits.sockets_max as usize].into_boxed_slice(),
            starved_until: 0,
            dry_until: 0,
        };
        area.reset();
        area
    }

    /// As when built: a quarter of the pages in the copy path's free
    /// list, the others in the pool, no piece out.
    pub(super) fn reset(&mut self) {
        let pages = self.refs.len();
        self.refs.fill(0);
        self.filling.fill(false);
        (self.pool_len, self.list_len, self.back_len, self.out_len) = (0, 0, 0, 0);
        for page in (0..pages as u32).rev() {
            self.copied[page as usize] = page as usize >= pages - pages / 4;
            self.home(page);
        }
        self.at.fill(None);
        self.silent.fill(false);
        (self.starved_until, self.dry_until) = (0, 0);
    }

    /// Pieces out, and pages with a piece out: the simulator's own count,
    /// for a check that does not trust the code under test.
    pub(super) fn out(&self) -> (usize, usize) {
        (self.out_len, self.refs.iter().filter(|n| **n > 0).count())
    }

    /// Put `page` back where it came from.
    fn home(&mut self, page: u32) {
        if self.copied[page as usize] {
            self.list[self.list_len] = page;
            self.list_len += 1;
        } else {
            self.pool[self.pool_len] = page;
            self.pool_len += 1;
        }
    }

    /// Read the returns back: every page that waits goes home.
    pub(super) fn read_returns(&mut self) {
        while let Some(top) = self.back_len.checked_sub(1) {
            self.back_len = top;
            self.home(self.back[top]);
        }
    }

    /// A page with no piece out and no socket filling it is returned;
    /// with as many returns waiting as fit, they are read back.
    fn settle(&mut self, page: u32) {
        if self.refs[page as usize] == 0 && !self.filling[page as usize] {
            self.back[self.back_len] = page;
            self.back_len += 1;
            if self.back_len >= self.back_most {
                self.read_returns();
            }
        }
    }

    /// `sock` fills no page any more, and no receive of it is silent: its
    /// connection is over, or a receive is armed on it anew.
    pub(super) fn leave(&mut self, sock: u32, closed: bool) {
        self.silent[sock as usize] = false;
        if !closed {
            return;
        }
        if let Some((page, _)) = self.at[sock as usize].take() {
            self.filling[page as usize] = false;
            self.settle(page);
        }
    }

    /// More bytes came for `sock`: a silent receive of it goes on.
    pub(super) fn wake(&mut self, sock: u32) {
        self.silent[sock as usize] = false;
    }

    /// The receive of `sock` reports nothing until it is armed anew or
    /// more bytes come.
    fn silence(&mut self, sock: u32) {
        self.silent[sock as usize] = true;
    }

    /// Where the next piece of `sock` goes: its page from where it is
    /// filled to, or a new page of the pool when `fresh` or that one is
    /// full. `None` when the pool has none, and for `wait` after that.
    fn place(&mut self, sock: u32, fresh: bool, now: u64, wait: u64) -> Option<(u32, u32)> {
        if let Some((page, filled)) = self.at[sock as usize] {
            if !fresh && filled < Limits::PAGE {
                return Some((page, filled));
            }
        }
        if now < self.starved_until {
            return None;
        }
        // The page it filled is left first: with nothing out of it, it
        // is a return like any other.
        if let Some((old, _)) = self.at[sock as usize].take() {
            self.filling[old as usize] = false;
            self.settle(old);
        }
        if self.pool_len == 0 {
            self.read_returns();
        }
        let Some(top) = self.pool_len.checked_sub(1) else {
            self.starved_until = now + wait;
            return None;
        };
        let page = self.pool[top];
        self.pool_len = top;
        self.filling[page as usize] = true;
        self.at[sock as usize] = Some((page, 0));
        Some((page, 0))
    }

    /// A page of the copy path's free list, or `None` when it has none,
    /// and for `wait` after that. The returns are read on the way out.
    fn copy_page(&mut self, now: u64, wait: u64) -> Option<u32> {
        if now >= self.dry_until {
            if let Some(top) = self.list_len.checked_sub(1) {
                self.list_len = top;
                return Some(self.list[top]);
            }
            self.dry_until = now + wait;
        }
        self.read_returns();
        None
    }

    /// Note a piece of `len` bytes at `off` of `page`.
    fn deliver(&mut self, page: u32, off: u32, len: u32) {
        self.refs[page as usize] += 1;
        self.out[self.out_len] = Piece { page, off, len };
        self.out_len += 1;
    }

    /// The piece `len` bytes of `id` from `off` is back. Stops the run
    /// for a piece that is not out.
    fn back(&mut self, id: BufId, off: u32, len: u32) {
        let page = id.0 - self.base;
        let piece = Piece { page, off, len };
        let found = self.out[..self.out_len].iter().position(|p| *p == piece);
        assert!(
            found.is_some(),
            "a piece returned that is not out: {len} bytes of page {page} from {off}"
        );
        let Some(at) = found else {
            return;
        };
        self.out_len -= 1;
        self.out[at] = self.out[self.out_len];
        self.refs[page as usize] -= 1;
        self.settle(page);
    }
}

impl Virtual {
    /// What an armed zero-copy receive of `sock` gives without a piece:
    /// its end, or `None` to wait. `Err` when there are bytes to deliver.
    fn recv_zc_without(&mut self, sock: Sock) -> Result<Option<Attempt>, usize> {
        let s = &self.socks[sock.0 as usize];
        if s.reset {
            return Ok(Some(Attempt::single(-ECONNRESET)));
        }
        if s.state != SockState::Open {
            return Ok(Some(Attempt::single(-EPIPE)));
        }
        if s.rd_shut {
            return Ok(Some(Attempt::single(0)));
        }
        if s.rx_len == 0 {
            let over = s.peer_closed || s.peer_wr_shut;
            return Ok(over.then(|| Attempt::single(0)));
        }
        if self.area.silent[sock.0 as usize] {
            return Ok(None);
        }
        if self.rng.chance(self.faults.reset) {
            self.reset_pair(sock.0);
            return Ok(Some(Attempt::single(-ECONNRESET)));
        }
        Err(s.rx_len)
    }

    /// One piece for an armed zero-copy receive of `sock` with `credit`
    /// bytes left: seeded in length, in the page the socket fills or,
    /// seeded, a new one, as a new packet starts one; now and then
    /// through the copy path.
    pub(super) fn attempt_recv_zc(
        &mut self,
        sock: Sock,
        credit: u32,
        pool: &mut [u8],
        limits: &Limits,
    ) -> Option<Attempt> {
        let avail = match self.recv_zc_without(sock) {
            Ok(end) => return end,
            Err(avail) => avail as u32,
        };
        let full = self.rng.chance(self.faults.ring_full);
        if full {
            self.ring_full = true;
            if self.rng.below(2) == 0 {
                // Nothing fitted in this call.
                return Some(Attempt::single(-ENOSPC));
            }
        }
        // A wait some steps long, and now and then much longer.
        let most = if self.rng.below(8) == 0 { 400 } else { 48 };
        let wait = STEP_NS * (16 + self.rng.below(most));
        // A packet that begins here starts a page, unless, one time in
        // eight, it was joined to the one before; no piece runs from one
        // packet into the next.
        let (head, waiting) = (
            self.socks[sock.0 as usize].rx_head,
            self.socks[sock.0 as usize].rx_len,
        );
        let (begins, firm, within) = self.packets.ahead(sock.0, head, waiting);
        // A packet after a send that ended a buffer is never joined; the
        // draw is made only for the others, so traces without stay as
        // they were.
        let begun = begins && (firm || self.rng.below(8) != 0);
        let (page, off) = if self.rng.chance(self.faults.copied) {
            let Some(page) = self.area.copy_page(self.now, wait) else {
                return Some(Attempt::single(-ENOMEM));
            };
            (page, 0)
        } else {
            // One packet in four starts a page, whatever room the last left.
            let fresh = self.rng.below(4) == 0 || begun;
            self.area.place(sock.0, fresh, self.now, wait)?
        };
        let mut n = (Limits::PAGE - off)
            .min(credit)
            .min(avail)
            .min(within as u32);
        if self.rng.chance(self.faults.partial_recv) {
            n = 1 + self.rng.below(u64::from(n)) as u32;
        }
        let id = BufId(limits.area_base() + page);
        let dst = limits.range(id, off, n);
        let q = &mut self.socks[sock.0 as usize];
        for k in 0..n as usize {
            pool[dst.start + k] = q.rx[(q.rx_head + k) % SOCK_BUF];
        }
        q.rx_head = (q.rx_head + n as usize) % SOCK_BUF;
        q.rx_len -= n as usize;
        self.area.deliver(page, off, n);
        if let Some((filled, at)) = self.area.at[sock.0 as usize].as_mut() {
            if *filled == page {
                *at = off + n;
            }
        }
        if full {
            // Part-way through a call: this piece, and then silence.
            self.area.silence(sock.0);
        }
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
        self.area.back(id, off, len);
        pool[limits.range(id, off, len)].fill(RETURNED);
    }
}
