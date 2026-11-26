// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The receive area of [`UringIo`]: the pool's pages registered with the
//! kernel for one receive queue of one interface, the zero-copy receive
//! that delivers pieces of them, and their return.
//!
//! The pages are the pool's own (`Limits::area_start`): the kernel pins
//! them and the card writes into them. The ring of returns is memory of
//! this module's, mapped for the purpose and given to the kernel as a
//! region. Both are the kernel's for as long as the ring lives; dropping
//! the ring takes every page back, whoever holds it.

use super::mapped::Mapped;
use super::split::Split;
use super::*;
use crate::io::refill::{Refill, Rqe};
use crate::io::{EBUSY, EINVAL, ENOMEM, ENOSPC, EOPNOTSUPP};
use core::sync::atomic::AtomicU32;

/// Linux `EPERM`, `ENODEV` and `EEXIST`, as a registration answers.
const EPERM: i32 = 1;
const ENODEV: i32 = 19;
const EEXIST: i32 = 17;

/// A registered receive area.
pub(super) struct Area {
    /// What names the registration in a receive and in a flush.
    id: u32,
    refill: Refill,
    /// A piece came since the last tick began.
    fed: bool,
    /// Keeps the ring of returns mapped; the kernel reads it.
    _region: Mapped,
}

/// What an `EINVAL` was for, as far as the card says. A card that does not
/// say may not split: the Sparks' does not report the setting while it is
/// off.
fn invalid(split: Option<Split>) -> &'static str {
    match split {
        Some(Split::Off) => {
            "the interface does not split headers from payload: ethtool -G IFACE tcp-data-split on"
        }
        Some(Split::On) => {
            "the interface splits headers from payload; the queue does not exist, \
             or an XDP program is attached"
        }
        Some(Split::Unknown) | None => {
            "the queue does not split headers from payload (ethtool -G IFACE \
             tcp-data-split on), or does not exist, or an XDP program is attached"
        }
    }
}

/// What the card says of its split, for every refusal.
fn said(split: Option<Split>) -> &'static str {
    match split {
        Some(Split::On) => "the card reports header-data split on",
        Some(Split::Off) => "the card reports header-data split off",
        Some(Split::Unknown) => "the card does not report header-data split",
        None => "the card could not be asked about header-data split",
    }
}

/// Name what a refused registration was refused for, with what the card
/// says of its split, asked over ethtool netlink.
fn refusal(errno: i32, limits: &Limits) -> io::Error {
    let (interface, queue) = limits.area_queue();
    let split = super::split::split_of(interface);
    let (kind, why) = match errno {
        EPERM => (
            io::ErrorKind::PermissionDenied,
            "registering a receive queue needs CAP_NET_ADMIN",
        ),
        ENODEV => (io::ErrorKind::NotFound, "there is no such interface"),
        // A queue past the interface's last is refused so too.
        EOPNOTSUPP => (
            io::ErrorKind::Unsupported,
            "the queue does not exist, or the interface's driver has no zero-copy receive",
        ),
        EBUSY | EEXIST => (
            io::ErrorKind::AddrInUse,
            "the queue is registered already, by this ring or another",
        ),
        EINVAL => (io::ErrorKind::InvalidInput, invalid(split)),
        _ => (io::ErrorKind::Other, "see io_uring_register(2)"),
    };
    let e = io::Error::from_raw_os_error(errno);
    let card = said(split);
    let named = io::Error::new(
        if errno == ENOMEM { e.kind() } else { kind },
        format!(
            "zero-copy receive on queue {queue} of interface {interface} was refused: {e}; {why}; {card}"
        ),
    );
    let bytes = limits.area_pages as usize * Limits::PAGE as usize;
    super::setup::name_memlock_limit(named, bytes)
}

impl Area {
    /// Register the pool's pages of the area for the queue `limits` names.
    /// `None` when `limits` asks for no area.
    pub(super) fn register(
        ring: &mut Ring,
        limits: &Limits,
        bufs: &mut [u8],
        pool_base: usize,
    ) -> io::Result<Option<Area>> {
        if limits.area_pages == 0 {
            return Ok(None);
        }
        if !ring.has_op(IORING_OP_RECV_ZC)? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this kernel has no io_uring zero-copy receive (IORING_OP_RECV_ZC, Linux 6.15; \
                 this backend is tested from 7.0)",
            ));
        }
        let entries = limits.area_returns();
        let page = Limits::PAGE as usize;
        let region_len =
            (entries as usize * core::mem::size_of::<Rqe>() + page).next_multiple_of(page);
        let mut region = Mapped::new(region_len, true)?;
        let base = region.as_mut_ptr();
        let start = unsafe { bufs.as_mut_ptr().add(pool_base + limits.area_start()) };
        let mut area = io_uring_zcrx_area_reg {
            addr: start as u64,
            len: u64::from(limits.area_pages) * u64::from(Limits::PAGE),
            ..Default::default()
        };
        let desc = io_uring_region_desc {
            user_addr: base as u64,
            size: region_len as u64,
            flags: IORING_MEM_REGION_TYPE_USER,
            ..Default::default()
        };
        let (if_idx, if_rxq) = limits.area_queue();
        let mut reg = io_uring_zcrx_ifq_reg {
            if_idx,
            if_rxq,
            rq_entries: entries,
            area_ptr: &raw mut area as u64,
            region_ptr: &raw const desc as u64,
            ..Default::default()
        };
        ring.register_zcrx(&mut reg)
            .map_err(|errno| refusal(errno, limits))?;
        // A kernel that cannot be asked to read the returns would leave a
        // backend with a full queue of them no way on: refused here.
        if ring.flush_zcrx(reg.zcrx_id).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the kernel cannot be asked to read a receive area's returns (needs Linux 7.0)",
            ));
        }
        let at = |off: u32| unsafe { base.add(off as usize) };
        let counters: (*const AtomicU32, *const AtomicU32) =
            (at(reg.offsets.head).cast(), at(reg.offsets.tail).cast());
        let rqes: *mut Rqe = at(reg.offsets.rqes).cast();
        // The kernel may have rounded the entries; what waits behind them
        // is as many again, so that a caller within its count never waits.
        let refill = unsafe {
            Refill::new(
                counters,
                rqes,
                reg.rq_entries,
                entries as usize,
                area.rq_area_token,
            )
        };
        Ok(Some(Area {
            id: reg.zcrx_id,
            refill,
            fed: false,
            _region: region,
        }))
    }

    /// Ask the kernel to read the returns. It can be asked: the area was
    /// refused at build where it could not.
    fn flush(&mut self, ring: &mut Ring) {
        // A refusal now is a ring being torn down: what it holds goes back
        // with it.
        let _read: Result<(), i32> = ring.flush_zcrx(self.id);
        self.refill.flushed();
    }

    /// The start of a tick: what waited goes into the ring, and the
    /// kernel reads it if that was asked for, or if returns lie unread in
    /// it while no piece came in the last tick or this one is to wait.
    ///
    /// The kernel reads the returns when the card wants pages. A card left
    /// with none wants none, and its queue stalls (milestone 31: for good
    /// in a run whose area ran dry). While pieces come the kernel is
    /// reading; when they stop, or the thread is about to sleep, it is
    /// asked to, at one call a pause.
    pub(super) fn turn(&mut self, ring: &mut Ring, wait: bool) {
        self.refill.drain();
        let idle = (wait || !self.fed) && self.refill.unread() > 0;
        if self.refill.wants_flush() || idle {
            self.flush(ring);
        }
        self.fed = false;
    }

    /// Return `len` bytes at `off` of the area; a return that found the
    /// ring full goes in at the next tick, and one that found the queue
    /// behind it full too has the kernel read the ring now.
    pub(super) fn give_back(&mut self, ring: &mut Ring, off: u64, len: u32) {
        if !self.refill.give(off, len) {
            self.flush(ring);
            let taken = self.refill.give(off, len);
            debug_assert!(taken, "a flushed ring has room");
        }
    }
}

impl UringIo {
    /// A zero-copy receive has a credit, and a ring with an area.
    pub(super) fn precheck_recv_zc(&self, sock: Sock, credit: u32) -> Result<(), i32> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        assert!(
            credit != 0,
            "a zero-copy receive is never armed with a credit of 0"
        );
        if self.area.is_none() {
            return Err(-ENOBUFS);
        }
        Ok(())
    }

    #[inline]
    pub(super) fn prep_armed_recv_zc(&mut self, i: u32, sock: Sock, credit: u32) -> io_uring_sqe {
        let s = &mut self.slots[i as usize];
        s.state = State::ArmedRecvZc;
        s.op_sock = Some(sock);
        let mut sqe = self.fixed(sock);
        sqe.opcode = IORING_OP_RECV_ZC;
        sqe.ioprio = IORING_RECV_MULTISHOT;
        sqe.len = credit;
        // The registration's id: `zcrx_ifq_idx` shares this field.
        sqe.file_index = self.area.as_ref().map_or(0, |a| a.id);
        sqe
    }

    /// One completion of a zero-copy receive: a piece, whose place is in
    /// the entry's second half, or the receive's end.
    #[inline]
    pub(super) fn on_recv_zc_cqe(&mut self, i: u32, c: &io_uring_cqe, out: &mut Completions) {
        let token = Token(self.slots[i as usize].token);
        if c.flags & IORING_CQE_F_MORE != 0 && c.res > 0 {
            let at = self.ring.extra() & ((1u64 << IORING_ZCRX_AREA_SHIFT) - 1);
            let page = BufId(self.limits.area_base() + (at / u64::from(Limits::PAGE)) as u32);
            let off = (at % u64::from(Limits::PAGE)) as u32;
            let piece = Completion::new(token, c.res).with_more(true);
            if let Some(area) = self.area.as_mut() {
                area.fed = true;
            }
            return out.push(piece.with_piece(page, off));
        }
        if c.flags & IORING_CQE_F_MORE != 0 {
            // Nothing to hand on, and more to follow.
            return;
        }
        if c.res == -ENOMEM {
            // The kernel's copy found no page: what was returned reaches
            // it only when it reads the returns.
            if let Some(area) = self.area.as_mut() {
                area.refill.ask_flush();
            }
        }
        if c.res == -ENOSPC {
            self.cq_full = true;
        }
        self.finish(i, c.res, out);
    }

    /// A tick begins: the completion queue has not been found full in it
    /// yet, and the area's returns move on.
    pub(super) fn begin_tick(&mut self, wait: bool) {
        self.cq_full = false;
        if let Some(area) = self.area.as_mut() {
            area.turn(&mut self.ring, wait);
        }
    }

    /// `Io::give_back_piece` on the ring.
    pub(super) fn return_piece(&mut self, page: BufId, off: u32, len: u32) {
        assert!(
            self.limits.is_page(page) && u64::from(off) + u64::from(len) <= u64::from(Limits::PAGE),
            "give_back_piece requires a piece of a page of the receive area"
        );
        let at = u64::from(page.0 - self.limits.area_base()) * u64::from(Limits::PAGE);
        if let Some(area) = self.area.as_mut() {
            area.give_back(&mut self.ring, at + u64::from(off), len);
        }
    }
}
