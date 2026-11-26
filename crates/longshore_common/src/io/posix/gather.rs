// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Gathered writes on the portable backend (`Op::FileWriteSpans`): one
//! `pwritev` over a vector filled from the list, which this backend runs
//! to completion before it takes the next operation, so one vector of
//! `Limits::gather_spans` entries, allocated at build, serves every write.
//! Also the world's teardown.

#[cfg(target_os = "linux")]
use super::abi_linux::*;
#[cfg(target_os = "macos")]
use super::abi_macos::*;
use super::errno::{self, to_linux_errno};
use super::Posix;
use crate::io::files::span_at;
use crate::io::{Limits, Op};
use core::ffi::c_void;

/// A run of memory: `struct iovec`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Iov {
    base: *const c_void,
    len: usize,
}

extern "C" {
    fn pwritev(fd: i32, iov: *const Iov, count: i32, offset: i64) -> isize;
}

/// The vector of a gathered write, `Limits::gather_spans` entries.
pub(super) fn vector(limits: &Limits) -> Box<[Iov]> {
    let none = Iov {
        base: core::ptr::null(),
        len: 0,
    };
    vec![none; limits.gather_spans as usize].into_boxed_slice()
}

impl Posix {
    /// An `Op::FileWriteSpans`, whose list was checked when it was
    /// submitted: write its spans to the file.
    pub(super) fn file_write_spans(&mut self, op: &Op, pool: &[u8], limits: &Limits) -> i32 {
        let Op::FileWriteSpans {
            slot,
            list,
            list_off,
            count,
            file_off,
            ..
        } = *op
        else {
            return -errno::linux::EINVAL;
        };
        let list_at = limits.range(list, list_off, count * 12).start;
        let fd = self.file_fds[slot.0 as usize];
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        let (Ok(off), Ok(n)) = (i64::try_from(file_off), i32::try_from(count)) else {
            return -errno::linux::EINVAL;
        };
        for k in 0..count {
            let (buf, at, len) = span_at(pool, list_at, k);
            let span = limits.range(buf, at, len);
            self.gather[k as usize] = Iov {
                base: pool[span].as_ptr().cast(),
                len: len as usize,
            };
        }
        loop {
            let wrote = unsafe { pwritev(fd, self.gather.as_ptr(), n, off) };
            if wrote >= 0 {
                return wrote as i32;
            }
            let e = os_errno();
            if e != OS_EINTR {
                return to_linux_errno(e);
            }
        }
    }
}

impl Drop for Posix {
    fn drop(&mut self) {
        for s in self.socks.iter_mut() {
            if s.fd >= 0 {
                self.poller.before_close(s);
                let _: i32 = unsafe { close(s.fd) };
                s.reset();
            }
        }
        for fd in self.file_fds.iter_mut() {
            if *fd >= 0 {
                let _: i32 = unsafe { close(*fd) };
                *fd = -1;
            }
        }
    }
}
