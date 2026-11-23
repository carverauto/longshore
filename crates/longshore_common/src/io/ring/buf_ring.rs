// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Page-aligned `io_uring` mapped buffer ring (`IORING_REGISTER_PBUF_RING`).

use super::Ring;
use crate::io::sys::*;
use crate::io::{BufId, Limits};
use core::mem::size_of;
use core::sync::atomic::{AtomicU16, Ordering};
use std::io;

/// Registered kernel buffer ring (`bgid = 0`) backed by page-aligned anonymous memory.
pub(crate) struct BufRing {
    ptr: *mut io_uring_buf,
    mmap_len: usize,
    mask: u16,
    tail: u16,
}

impl BufRing {
    /// Allocate page-aligned memory, register it with `ring`, and populate all
    /// `limits.ring_buffers` buffers from the pool.
    pub(crate) fn new(
        ring: &mut Ring,
        limits: &Limits,
        bufs: &[u8],
        pool_base: usize,
    ) -> io::Result<Self> {
        let count = limits.ring_buffers;
        assert!(count > 0 && count <= 32_768);
        let entries = count.next_power_of_two();
        let mmap_len = entries as usize * size_of::<io_uring_buf>();
        let raw = unsafe {
            mmap(
                core::ptr::null_mut(),
                mmap_len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let reg = io_uring_buf_reg {
            ring_addr: raw as u64,
            ring_entries: entries,
            bgid: 0,
            pad: 0,
            resv: [0; 3],
        };
        if let Err(e) = ring.register_pbuf_ring(&reg) {
            let _: i32 = unsafe { munmap(raw, mmap_len) };
            return Err(e);
        }
        let mut br = Self {
            ptr: raw.cast(),
            mmap_len,
            mask: (entries - 1) as u16,
            tail: 0,
        };
        let base_addr = bufs.as_ptr() as u64 + pool_base as u64;
        let ring_base = limits.ring_base();
        for bid in 0..count {
            let (off, len) = limits.span(BufId(ring_base + bid));
            br.push_buf(base_addr + off as u64, len as u32, bid as u16);
        }
        Ok(br)
    }

    /// Return a buffer to the ring and publish the updated tail (`Ordering::Release`)
    /// without a system call.
    #[inline]
    pub(crate) fn push_buf(&mut self, addr: u64, len: u32, bid: u16) {
        let idx = (self.tail & self.mask) as usize;
        unsafe {
            let slot = self.ptr.add(idx);
            // Field-wise writes: `bufs[0].resv` (byte offset 14) aliases `tail`.
            core::ptr::addr_of_mut!((*slot).addr).write(addr);
            core::ptr::addr_of_mut!((*slot).len).write(len);
            core::ptr::addr_of_mut!((*slot).bid).write(bid);
            self.tail = self.tail.wrapping_add(1);
            let tail_ptr = core::ptr::addr_of_mut!((*self.ptr).resv);
            AtomicU16::from_ptr(tail_ptr).store(self.tail, Ordering::Release);
        }
    }
}

impl Drop for BufRing {
    fn drop(&mut self) {
        let _: i32 = unsafe { munmap(self.ptr.cast(), self.mmap_len) };
    }
}
