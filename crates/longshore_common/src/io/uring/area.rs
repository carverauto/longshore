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

use super::*;
use crate::io::refill::{Refill, Rqe};
use crate::io::{EBUSY, EINVAL, ENOMEM, ENOSPC, EOPNOTSUPP};
use core::ffi::c_void;
use core::sync::atomic::AtomicU32;

/// Linux `EPERM`, `ENODEV` and `EEXIST`, as a registration answers.
const EPERM: i32 = 1;
const ENODEV: i32 = 19;
const EEXIST: i32 = 17;

/// Memory mapped for the ring of returns.
struct Region {
    ptr: *mut c_void,
    len: usize,
}

impl Region {
    fn new(len: usize) -> io::Result<Region> {
        let ptr = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_POPULATE,
                -1,
                0,
            )
        };
        if ptr == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Region { ptr, len })
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        let _: i32 = unsafe { munmap(self.ptr, self.len) };
    }
}

/// A registered receive area.
pub(super) struct Area {
    /// What names the registration in a receive and in a flush.
    id: u32,
    refill: Refill,
    /// Keeps the ring of returns mapped; the kernel reads it.
    _region: Region,
}

/// Name what a refused registration was refused for.
fn refusal(errno: i32, limits: &Limits) -> io::Error {
    let (interface, queue) = limits.area_queue();
    let (kind, why) = match errno {
        EPERM => (
            io::ErrorKind::PermissionDenied,
            "registering a receive queue needs CAP_NET_ADMIN",
        ),
        ENODEV => (io::ErrorKind::NotFound, "there is no such interface"),
        EOPNOTSUPP => (
            io::ErrorKind::Unsupported,
            "the interface's driver has no zero-copy receive",
        ),
        EBUSY | EEXIST => (
            io::ErrorKind::AddrInUse,
            "the queue is registered already, by this ring or another",
        ),
        EINVAL => (
            io::ErrorKind::InvalidInput,
            "the queue does not split headers from payload (ethtool -G IFACE \
             tcp-data-split on), or does not exist, or an XDP program is attached",
        ),
        _ => (io::ErrorKind::Other, "see io_uring_register(2)"),
    };
    let e = io::Error::from_raw_os_error(errno);
    let named = io::Error::new(
        if errno == ENOMEM { e.kind() } else { kind },
        format!(
            "zero-copy receive on queue {queue} of interface {interface} was refused: {e}; {why}"
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
        let region = Region::new(region_len)?;
        let start = unsafe { bufs.as_mut_ptr().add(pool_base + limits.area_start()) };
        let mut area = io_uring_zcrx_area_reg {
            addr: start as u64,
            len: u64::from(limits.area_pages) * u64::from(Limits::PAGE),
            ..Default::default()
        };
        let desc = io_uring_region_desc {
            user_addr: region.ptr as u64,
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
        let at = |off: u32| unsafe { region.ptr.cast::<u8>().add(off as usize) };
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
    /// kernel reads it if that was asked for.
    pub(super) fn turn(&mut self, ring: &mut Ring) {
        self.refill.drain();
        if self.refill.wants_flush() {
            self.flush(ring);
        }
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
    pub(super) fn begin_tick(&mut self) {
        self.cq_full = false;
        if let Some(area) = self.area.as_mut() {
            area.turn(&mut self.ring);
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
