// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Minimal io_uring instance for a single submitting thread.
//!
//! `push` writes an SQE with no syscall. `enter` publishes everything pushed
//! since the last call and reaps in one `io_uring_enter`. `pop` reads one CQE.

use crate::io::sys::*;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicU32, Ordering};
use std::io;

pub(crate) mod buf_ring;

struct Map {
    ptr: *mut c_void,
    len: usize,
}

impl Map {
    fn new(fd: c_int, len: usize, off: i64) -> io::Result<Map> {
        let ptr = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED | MAP_POPULATE,
                fd,
                off,
            )
        };
        if ptr == MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Map { ptr, len })
    }
    unsafe fn at<T>(&self, off: u32) -> *mut T {
        self.ptr.cast::<u8>().add(off as usize).cast()
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        unsafe { munmap(self.ptr, self.len) };
    }
}

pub struct Ring {
    fd: c_int,
    pub params: io_uring_params,
    _sq_map: Map,
    _cq_map: Option<Map>,
    _sqe_map: Map,
    sq_head: *const AtomicU32,
    sq_tail: *const AtomicU32,
    sq_mask: u32,
    sq_entries: u32,
    sqes: *mut io_uring_sqe,
    /// Tail we have written SQEs up to but not yet published to the kernel.
    sq_tail_local: u32,
    /// Tail as last published; `sq_tail_local - sq_tail_published` is pending.
    sq_tail_published: u32,
    cq_head: *const AtomicU32,
    cq_tail: *const AtomicU32,
    cq_mask: u32,
    cqes: *const io_uring_cqe,
    /// Number of `io_uring_enter` calls made, for benchmarks.
    pub enters: u64,
}

/// The submission queue has no free slot; call `enter` first.
#[derive(Debug)]
pub struct SqFull;

impl Ring {
    /// Create a ring. Must be called on the thread that will submit when
    /// `IORING_SETUP_SINGLE_ISSUER` is set.
    pub fn new(entries: u32, flags: u32) -> io::Result<Ring> {
        Self::setup(entries, None, flags)
    }

    /// Create a ring with an explicit completion queue size (`IORING_SETUP_CQSIZE`).
    pub fn with_cq(entries: u32, cq_entries: u32, flags: u32) -> io::Result<Ring> {
        Self::setup(entries, Some(cq_entries), flags | IORING_SETUP_CQSIZE)
    }

    fn setup(entries: u32, cq_entries: Option<u32>, flags: u32) -> io::Result<Ring> {
        let mut p = io_uring_params {
            flags,
            cq_entries: cq_entries.unwrap_or(0),
            ..Default::default()
        };
        let fd = unsafe { syscall(SYS_IO_URING_SETUP, entries, &mut p as *mut io_uring_params) }
            as c_int;
        if fd < 0 {
            let e = io::Error::last_os_error();
            return Err(io::Error::new(
                e.kind(),
                format!(
                    "io_uring_setup(entries={entries}, cq_entries={}, flags={flags:#x}) failed: {e}; {}",
                    cq_entries.unwrap_or(0),
                    setup_hint(&e)
                ),
            ));
        }
        let res = Self::map(fd, p);
        if res.is_err() {
            unsafe { close(fd) };
        }
        res
    }

    fn map(fd: c_int, p: io_uring_params) -> io::Result<Ring> {
        let sq_len = p.sq_off.array as usize + p.sq_entries as usize * 4;
        let cq_len =
            p.cq_off.cqes as usize + p.cq_entries as usize * core::mem::size_of::<io_uring_cqe>();
        let single = p.features & IORING_FEAT_SINGLE_MMAP != 0;
        let sq_map = Map::new(
            fd,
            if single { sq_len.max(cq_len) } else { sq_len },
            IORING_OFF_SQ_RING,
        )?;
        let cq_map = if single {
            None
        } else {
            Some(Map::new(fd, cq_len, IORING_OFF_CQ_RING)?)
        };
        let sqe_map = Map::new(
            fd,
            p.sq_entries as usize * core::mem::size_of::<io_uring_sqe>(),
            IORING_OFF_SQES,
        )?;
        unsafe {
            let cq = cq_map.as_ref().unwrap_or(&sq_map);
            let sq_mask = *sq_map.at::<u32>(p.sq_off.ring_mask);
            // Identity-map the indirection array once; we always fill slots in order.
            let array = sq_map.at::<u32>(p.sq_off.array);
            for i in 0..p.sq_entries {
                *array.add(i as usize) = i;
            }
            let sq_tail: *const AtomicU32 = sq_map.at(p.sq_off.tail);
            let tail = (*sq_tail).load(Ordering::Relaxed);
            Ok(Ring {
                fd,
                params: p,
                sq_head: sq_map.at(p.sq_off.head),
                sq_tail,
                sq_mask,
                sq_entries: p.sq_entries,
                sqes: sqe_map.ptr.cast(),
                sq_tail_local: tail,
                sq_tail_published: tail,
                cq_head: cq.at(p.cq_off.head),
                cq_tail: cq.at(p.cq_off.tail),
                cq_mask: *cq.at::<u32>(p.cq_off.ring_mask),
                cqes: cq.at(p.cq_off.cqes),
                _sq_map: sq_map,
                _cq_map: cq_map,
                _sqe_map: sqe_map,
                enters: 0,
            })
        }
    }

    /// Put another file where the ring's descriptor is, so that every
    /// later `enter` is refused by the kernel. The ring itself lives on
    /// through its mappings, with whatever it had in flight.
    #[cfg(feature = "fault-injection")]
    pub fn lose(&mut self) {
        use std::os::fd::AsRawFd;
        let null = std::fs::File::open("/dev/null").expect("open /dev/null");
        let replaced = unsafe { dup2(null.as_raw_fd(), self.fd) };
        assert_eq!(replaced, self.fd, "dup2 over the ring's descriptor");
    }

    /// Queue one SQE. No syscall.
    #[inline]
    pub fn push(&mut self, sqe: io_uring_sqe) -> Result<(), SqFull> {
        let head = unsafe { (*self.sq_head).load(Ordering::Acquire) };
        if self.sq_tail_local.wrapping_sub(head) >= self.sq_entries {
            return Err(SqFull);
        }
        unsafe {
            self.sqes
                .add((self.sq_tail_local & self.sq_mask) as usize)
                .write(sqe)
        };
        self.sq_tail_local = self.sq_tail_local.wrapping_add(1);
        Ok(())
    }

    /// SQEs pushed but not yet handed to the kernel.
    #[inline]
    pub fn pending(&self) -> u32 {
        self.sq_tail_local.wrapping_sub(self.sq_tail_published)
    }

    /// Submit everything pushed and wait for at least `min_complete`
    /// completions, in one syscall. Returns the number of SQEs consumed.
    pub fn enter(&mut self, min_complete: u32) -> io::Result<u32> {
        let to_submit = self.pending();
        unsafe { (*self.sq_tail).store(self.sq_tail_local, Ordering::Release) };
        self.sq_tail_published = self.sq_tail_local;
        // GETEVENTS is always set: with DEFER_TASKRUN, completions are only
        // posted when the submitting thread asks for them.
        let flags = IORING_ENTER_GETEVENTS;
        loop {
            self.enters += 1;
            let r = unsafe {
                syscall(
                    SYS_IO_URING_ENTER,
                    self.fd,
                    to_submit,
                    min_complete,
                    flags,
                    core::ptr::null::<c_void>(),
                    0usize,
                )
            };
            if r >= 0 {
                return Ok(r as u32);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// Take one completion, if any.
    #[inline]
    pub fn pop(&mut self) -> Option<io_uring_cqe> {
        unsafe {
            let head = (*self.cq_head).load(Ordering::Relaxed);
            let tail = (*self.cq_tail).load(Ordering::Acquire);
            if head == tail {
                return None;
            }
            let cqe = self.cqes.add((head & self.cq_mask) as usize).read();
            (*self.cq_head).store(head.wrapping_add(1), Ordering::Release);
            Some(cqe)
        }
    }

    /// Register buffers for `IORING_RECVSEND_FIXED_BUF` / fixed read-write.
    pub fn register_buffers(&mut self, bufs: &[iovec]) -> io::Result<()> {
        self.register(
            IORING_REGISTER_BUFFERS,
            bufs.as_ptr().cast(),
            bufs.len() as u32,
        )
    }

    /// Register file descriptors; index `i` is then usable with `IOSQE_FIXED_FILE`.
    pub fn register_files(&mut self, fds: &[i32]) -> io::Result<()> {
        self.register(IORING_REGISTER_FILES, fds.as_ptr().cast(), fds.len() as u32)
    }

    /// Confine `IORING_FILE_INDEX_ALLOC` allocations to `off..off + len`.
    pub fn register_file_alloc_range(&mut self, off: u32, len: u32) -> io::Result<()> {
        let range = io_uring_file_index_range { off, len, resv: 0 };
        self.register(
            IORING_REGISTER_FILE_ALLOC_RANGE,
            (&range as *const io_uring_file_index_range).cast(),
            0,
        )
    }

    /// Register a mapped buffer ring (`nr_args == 1` per Linux 6.1).
    pub fn register_pbuf_ring(&mut self, reg: &io_uring_buf_reg) -> io::Result<()> {
        self.register(
            IORING_REGISTER_PBUF_RING,
            (reg as *const io_uring_buf_reg).cast(),
            1,
        )
    }

    /// Install `fd` at index `slot` of the registered file table.
    pub fn update_file(&mut self, slot: u32, fd: i32) -> io::Result<()> {
        self.update_file_raw(slot, fd)
            .map_err(io::Error::from_raw_os_error)
    }

    /// Install `fd` at index `slot` of the registered file table, returning the
    /// raw OS errno without heap allocation on error.
    pub fn update_file_raw(&mut self, slot: u32, fd: i32) -> Result<(), i32> {
        let fds = [fd];
        let up = io_uring_files_update {
            offset: slot,
            resv: 0,
            fds: fds.as_ptr() as u64,
        };
        self.register_raw(
            IORING_REGISTER_FILES_UPDATE,
            (&up as *const io_uring_files_update).cast(),
            1,
        )
    }

    fn register_raw(&mut self, op: u32, arg: *const c_void, n: u32) -> Result<(), i32> {
        let r = unsafe { syscall(SYS_IO_URING_REGISTER, self.fd, op, arg, n) };
        if r < 0 {
            return Err(io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(crate::io::EINVAL));
        }
        Ok(())
    }

    fn register(&mut self, op: u32, arg: *const c_void, n: u32) -> io::Result<()> {
        if let Err(errno) = self.register_raw(op, arg, n) {
            let e = io::Error::from_raw_os_error(errno);
            return Err(io::Error::new(
                e.kind(),
                format!("io_uring_register(op={op}, n={n}) failed: {e}"),
            ));
        }
        Ok(())
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

fn setup_hint(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(1) | Some(13) => "io_uring is blocked (seccomp profile, or kernel.io_uring_disabled is not 0)",
        Some(22) => "the kernel rejected the setup flags; SINGLE_ISSUER needs Linux 6.0 and DEFER_TASKRUN needs 6.1",
        Some(38) => "this kernel has no io_uring support",
        Some(12) => "not enough locked memory; raise RLIMIT_MEMLOCK",
        _ => "see io_uring_setup(2)",
    }
}
