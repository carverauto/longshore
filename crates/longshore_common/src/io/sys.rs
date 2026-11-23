// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Kernel ABI for io_uring, plus the libc symbols this crate needs.
//!
//! No `libc` crate: `std` already links the C library, so the symbols are
//! declared here directly. Values are transcribed from the uapi header and are
//! identical on x86-64 and AArch64 (io_uring syscalls use the unified numbers).

use core::ffi::{c_int, c_long, c_uint, c_void};
use core::mem::{offset_of, size_of};

mod sock;
pub use sock::*;

pub const SYS_IO_URING_SETUP: c_long = 425;
pub const SYS_IO_URING_ENTER: c_long = 426;
pub const SYS_IO_URING_REGISTER: c_long = 427;

// io_uring_setup flags. Kernel that introduced each is noted.
pub const IORING_SETUP_CQSIZE: u32 = 1 << 3;
pub const IORING_SETUP_CLAMP: u32 = 1 << 4;
pub const IORING_SETUP_SUBMIT_ALL: u32 = 1 << 7; // 5.18
pub const IORING_SETUP_COOP_TASKRUN: u32 = 1 << 8; // 5.19
pub const IORING_SETUP_SINGLE_ISSUER: u32 = 1 << 12; // 6.0
pub const IORING_SETUP_DEFER_TASKRUN: u32 = 1 << 13; // 6.1

pub const IORING_FEAT_SINGLE_MMAP: u32 = 1 << 0;
pub const IORING_FEAT_NODROP: u32 = 1 << 1;

pub const IORING_ENTER_GETEVENTS: u32 = 1 << 0;

pub const IORING_OFF_SQ_RING: i64 = 0;
pub const IORING_OFF_CQ_RING: i64 = 0x800_0000;
pub const IORING_OFF_SQES: i64 = 0x1000_0000;

pub const IORING_REGISTER_BUFFERS: c_uint = 0; // 5.1
pub const IORING_REGISTER_FILES: c_uint = 2; // 5.1
pub const IORING_REGISTER_FILES_UPDATE: c_uint = 6; // 5.1
pub const IORING_REGISTER_PBUF_RING: c_uint = 22; // 5.19
pub const IORING_UNREGISTER_PBUF_RING: c_uint = 23; // 5.19
pub const IORING_REGISTER_FILE_ALLOC_RANGE: c_uint = 25; // 6.0

// Opcodes.
pub const IORING_OP_NOP: u8 = 0;
pub const IORING_OP_FSYNC: u8 = 3;
pub const IORING_OP_READ_FIXED: u8 = 4;
pub const IORING_OP_WRITE_FIXED: u8 = 5;
pub const IORING_OP_TIMEOUT: u8 = 11;
pub const IORING_OP_TIMEOUT_REMOVE: u8 = 12;
pub const IORING_OP_ACCEPT: u8 = 13;
pub const IORING_OP_ASYNC_CANCEL: u8 = 14;
pub const IORING_OP_CONNECT: u8 = 16;
pub const IORING_OP_FALLOCATE: u8 = 17;
pub const IORING_OP_OPENAT: u8 = 18;
pub const IORING_OP_CLOSE: u8 = 19;
pub const IORING_OP_STATX: u8 = 21;
pub const IORING_OP_READ: u8 = 22;
pub const AT_FDCWD: i32 = -100;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
#[cfg(target_arch = "x86_64")]
pub const O_DIRECT: u32 = 0o0040000;
#[cfg(target_arch = "x86_64")]
pub const O_DIRECTORY: u32 = 0o0200000;
#[cfg(target_arch = "aarch64")]
pub const O_DIRECTORY: u32 = 0o0040000;
#[cfg(target_arch = "aarch64")]
pub const O_DIRECT: u32 = 0o0200000;
pub const O_RDONLY_CLOEXEC: u32 = 0o2000000;
pub const IORING_OP_SEND: u8 = 26;
pub const IORING_OP_RECV: u8 = 27;
pub const IORING_OP_SHUTDOWN: u8 = 34; // 5.11
pub const IORING_OP_RENAMEAT: u8 = 35;
pub const IORING_OP_UNLINKAT: u8 = 36;
pub const IORING_OP_MKDIRAT: u8 = 37;
pub const IORING_OP_SOCKET: u8 = 45; // 5.19
pub const IORING_OP_URING_CMD: u8 = 46; // 5.19
pub const IORING_OP_SEND_ZC: u8 = 47; // 6.0

pub const SOCKET_URING_OP_GETSOCKOPT: u64 = 2; // 6.7

pub const IOSQE_FIXED_FILE: u8 = 1 << 0;
pub const IOSQE_IO_LINK: u8 = 1 << 2;
pub const IOSQE_IO_HARDLINK: u8 = 1 << 3;
pub const IOSQE_BUFFER_SELECT: u8 = 1 << 5;

pub const IORING_ACCEPT_MULTISHOT: u16 = 1 << 0;
pub const IORING_RECV_MULTISHOT: u16 = 1 << 1;
pub const IORING_FILE_INDEX_ALLOC: u32 = !0u32;

pub const IORING_FSYNC_DATASYNC: u32 = 1 << 0;
pub const FALLOC_FL_KEEP_SIZE: u32 = 0x01;
pub const FALLOC_FL_PUNCH_HOLE: u32 = 0x02;
pub const STATX_BASIC_STATS: u32 = 0x07ff;

pub const RLIMIT_NOFILE: c_int = 7;
pub const RLIMIT_MEMLOCK: c_int = 8;
pub const RLIM_INFINITY: u64 = !0u64;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct rlimit {
    pub rlim_cur: u64,
    pub rlim_max: u64,
}

// sqe.ioprio flags for send/recv.
pub const IORING_RECVSEND_FIXED_BUF: u16 = 1 << 2; // 6.0
pub const IORING_SEND_ZC_REPORT_USAGE: u16 = 1 << 3; // 6.2

pub const IORING_CQE_F_BUFFER: u32 = 1 << 0;
pub const IORING_CQE_F_MORE: u32 = 1 << 1;
pub const IORING_CQE_F_NOTIF: u32 = 1 << 3;
pub const IORING_CQE_BUFFER_SHIFT: u32 = 16;
/// Set in `cqe.res` of a notification CQE when the kernel copied the data
/// instead of sending it zero-copy (requires `IORING_SEND_ZC_REPORT_USAGE`).
pub const IORING_NOTIF_USAGE_ZC_COPIED: u32 = 1 << 31;

pub const EFD_CLOEXEC: c_int = 0o2000000;
pub const CLOCK_MONOTONIC: c_int = 1;
pub const CLOCK_REALTIME: c_int = 0;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_sqe {
    pub opcode: u8,
    pub flags: u8,
    pub ioprio: u16,
    pub fd: i32,
    pub off: u64,
    pub addr: u64,
    pub len: u32,
    pub op_flags: u32,
    pub user_data: u64,
    pub buf_index: u16,
    pub personality: u16,
    pub file_index: u32,
    pub addr3: u64,
    pub pad2: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct io_uring_cqe {
    pub user_data: u64,
    pub res: i32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_sqring_offsets {
    pub head: u32,
    pub tail: u32,
    pub ring_mask: u32,
    pub ring_entries: u32,
    pub flags: u32,
    pub dropped: u32,
    pub array: u32,
    pub resv1: u32,
    pub user_addr: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_cqring_offsets {
    pub head: u32,
    pub tail: u32,
    pub ring_mask: u32,
    pub ring_entries: u32,
    pub overflow: u32,
    pub cqes: u32,
    pub flags: u32,
    pub resv1: u32,
    pub user_addr: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_params {
    pub sq_entries: u32,
    pub cq_entries: u32,
    pub flags: u32,
    pub sq_thread_cpu: u32,
    pub sq_thread_idle: u32,
    pub features: u32,
    pub wq_fd: u32,
    pub resv: [u32; 3],
    pub sq_off: io_sqring_offsets,
    pub cq_off: io_cqring_offsets,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct iovec {
    pub iov_base: *mut c_void,
    pub iov_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_files_update {
    pub offset: u32,
    pub resv: u32,
    pub fds: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_file_index_range {
    pub off: u32,
    pub len: u32,
    pub resv: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_buf_reg {
    pub ring_addr: u64,
    pub ring_entries: u32,
    pub bgid: u16,
    pub pad: u16,
    pub resv: [u64; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_buf {
    pub addr: u64,
    pub len: u32,
    pub bid: u16,
    /// `io_uring_buf_ring.tail` aliases `bufs[0].resv` at byte offset 14.
    pub resv: u16,
}

/// `struct __kernel_timespec`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct timespec {
    pub sec: i64,
    pub nsec: i64,
}

const _: () = {
    assert!(size_of::<io_uring_sqe>() == 64);
    assert!(size_of::<io_uring_cqe>() == 16);
    assert!(size_of::<io_uring_params>() == 120);
    assert!(size_of::<io_uring_file_index_range>() == 16);
    assert!(size_of::<io_uring_buf_reg>() == 40);
    assert!(size_of::<io_uring_buf>() == 16);
    assert!(offset_of!(io_uring_buf, resv) == 14);
};

pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const MAP_SHARED: c_int = 0x01;
pub const MAP_PRIVATE: c_int = 0x02;
pub const MAP_ANONYMOUS: c_int = 0x20;
pub const MAP_POPULATE: c_int = 0x8000;
pub const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

extern "C" {
    pub fn syscall(num: c_long, ...) -> c_long;
    pub fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, len: usize) -> c_int;
    pub fn close(fd: c_int) -> c_int;
    pub fn eventfd(initval: c_uint, flags: c_int) -> c_int;
    pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    pub fn dup(fd: c_int) -> c_int;
    #[cfg(feature = "fault-injection")]
    pub fn dup2(old: c_int, new: c_int) -> c_int;
    pub fn clock_gettime(clk: c_int, ts: *mut timespec) -> c_int;
    pub fn getrlimit(resource: c_int, rlim: *mut rlimit) -> c_int;
}

/// Map portable [`crate::io::OpenFlags`] to Linux architecture-specific `openat` flags
/// without `O_CLOEXEC` (refused for registered `io_uring` fixed file slots).
pub fn open_flags_linux(f: crate::io::OpenFlags) -> u32 {
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
    out
}
