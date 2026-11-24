// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Blocking POSIX file operations for [`Posix`] (macOS and Linux).

#[cfg(target_os = "linux")]
use super::abi_linux::*;
#[cfg(target_os = "macos")]
use super::abi_macos::*;
use super::errno::{self, to_linux_errno};
use super::Posix;
use crate::io::files::STATX_SIZE;
use crate::io::{FileSlot, Limits, Op, OpenFlags};
use core::ffi::{c_char, c_int, c_void};

#[path = "files_space.rs"]
mod space;

#[cfg(target_os = "macos")]
fn open_flags(f: OpenFlags) -> c_int {
    let mut out = match (f.read, f.write) {
        (true, true) => O_RDWR,
        (false, true) => O_WRONLY,
        (true, false) | (false, false) => O_RDONLY,
    };
    if f.create {
        out |= O_CREAT;
    }
    if f.exclusive {
        out |= O_EXCL;
    }
    if f.empty {
        out |= O_TRUNC;
    }
    if f.directory {
        out |= O_DIRECTORY;
    }
    out | O_CLOEXEC
}

#[cfg(target_os = "linux")]
fn open_flags(f: OpenFlags) -> c_int {
    let mut out = match (f.read, f.write) {
        (true, true) => O_RDWR,
        (false, true) => O_WRONLY,
        (true, false) | (false, false) => O_RDONLY,
    };
    if f.create {
        out |= O_CREAT;
    }
    if f.exclusive {
        out |= O_EXCL;
    }
    if f.empty {
        out |= O_TRUNC;
    }
    if f.direct {
        out |= O_DIRECT;
    }
    if f.directory {
        out |= O_DIRECTORY;
    }
    out | O_CLOEXEC
}

impl Posix {
    pub(super) fn attempt_file_op(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
    ) -> Option<i32> {
        match *op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                let path: *const c_char = pool[r].as_ptr().cast();
                Some(self.file_open(slot, path, flags))
            }
            Op::FileClose { slot } => Some(self.file_close(slot)),
            Op::FileRead {
                slot,
                buf,
                off,
                len,
                file_off,
            } => Some(self.file_read(slot, pool, limits.range(buf, off, len), file_off)),
            Op::FileWrite {
                slot,
                buf,
                off,
                len,
                file_off,
            } => Some(self.file_write(slot, pool, limits.range(buf, off, len), file_off)),
            Op::FileSync { slot, datasync } => Some(self.file_sync(slot, datasync)),
            Op::FileReserve {
                slot,
                file_off,
                len,
                keep_size,
            } => Some(self.file_reserve(slot, file_off, len, keep_size)),
            Op::FilePunch {
                slot,
                file_off,
                len,
            } => Some(self.file_punch(slot, file_off, len)),
            Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => Some(Self::attempt_path_op(op, pool, limits)),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. } => unreachable!("non-file op"),
        }
    }

    /// Runs a file operation that names paths rather than an open slot.
    fn attempt_path_op(op: &Op, pool: &mut [u8], limits: &Limits) -> i32 {
        match *op {
            Op::FileStatus {
                path_buf,
                path_off,
                path_len,
                out_buf,
                out_off,
            } => {
                let pr = limits.range(path_buf, path_off, path_len);
                let or = limits.range(out_buf, out_off, STATX_SIZE as u32);
                Self::file_status(pool, pr, or)
            }
            Op::FileRename {
                old_buf,
                old_off,
                old_len,
                new_buf,
                new_off,
                new_len,
            } => {
                let r1 = limits.range(old_buf, old_off, old_len);
                let r2 = limits.range(new_buf, new_off, new_len);
                Self::file_rename(pool[r1].as_ptr().cast(), pool[r2].as_ptr().cast())
            }
            Op::FileUnlink {
                path_buf,
                path_off,
                path_len,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                Self::file_unlink(pool[r].as_ptr().cast())
            }
            Op::FileMkdir {
                path_buf,
                path_off,
                path_len,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                Self::file_mkdir(pool[r].as_ptr().cast())
            }
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. } => unreachable!("not a path op"),
        }
    }

    fn file_open(&mut self, slot: FileSlot, path: *const c_char, flags: OpenFlags) -> i32 {
        let mode: c_int = if flags.create { 0o666 } else { 0 };
        let oflags = open_flags(flags);
        let fd = loop {
            #[cfg(target_os = "macos")]
            let fd = unsafe { open(path, oflags, mode) };
            #[cfg(target_os = "linux")]
            let fd = unsafe { openat(AT_FDCWD, path, oflags, mode) };
            if fd >= 0 {
                break fd;
            }
            let e = os_errno();
            if e != OS_EINTR {
                return to_linux_errno(e);
            }
        };
        #[cfg(target_os = "macos")]
        if flags.direct && unsafe { fcntl(fd, F_NOCACHE, 1) } < 0 {
            let e = os_errno();
            let _: i32 = unsafe { close(fd) };
            return to_linux_errno(e);
        }
        self.file_fds[slot.0 as usize] = fd;
        0
    }

    fn file_close(&mut self, slot: FileSlot) -> i32 {
        let fd = core::mem::replace(&mut self.file_fds[slot.0 as usize], -1);
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        if unsafe { close(fd) } < 0 {
            return to_linux_errno(os_errno());
        }
        0
    }

    fn file_read(
        &self,
        slot: FileSlot,
        pool: &mut [u8],
        range: core::ops::Range<usize>,
        file_off: u64,
    ) -> i32 {
        let fd = self.file_fds[slot.0 as usize];
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        let Ok(off) = i64::try_from(file_off) else {
            return -errno::linux::EINVAL;
        };
        let ptr: *mut c_void = pool[range.clone()].as_mut_ptr().cast();
        let len = range.len();
        loop {
            let n = unsafe { pread(fd, ptr, len, off) };
            if n >= 0 {
                return n as i32;
            }
            let e = os_errno();
            if e != OS_EINTR {
                return to_linux_errno(e);
            }
        }
    }

    fn file_write(
        &self,
        slot: FileSlot,
        pool: &[u8],
        range: core::ops::Range<usize>,
        file_off: u64,
    ) -> i32 {
        let fd = self.file_fds[slot.0 as usize];
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        let Ok(off) = i64::try_from(file_off) else {
            return -errno::linux::EINVAL;
        };
        let ptr: *const c_void = pool[range.clone()].as_ptr().cast();
        let len = range.len();
        loop {
            let n = unsafe { pwrite(fd, ptr, len, off) };
            if n >= 0 {
                return n as i32;
            }
            let e = os_errno();
            if e != OS_EINTR {
                return to_linux_errno(e);
            }
        }
    }

    fn file_sync(&self, slot: FileSlot, datasync: bool) -> i32 {
        let fd = self.file_fds[slot.0 as usize];
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        loop {
            #[cfg(target_os = "macos")]
            let r = {
                let _: bool = datasync;
                unsafe { fcntl(fd, F_FULLFSYNC) }
            };
            #[cfg(target_os = "linux")]
            let r = unsafe {
                if datasync {
                    fdatasync(fd)
                } else {
                    fsync(fd)
                }
            };
            if r == 0 {
                return 0;
            }
            let e = os_errno();
            if e != OS_EINTR {
                return to_linux_errno(e);
            }
        }
    }
}
