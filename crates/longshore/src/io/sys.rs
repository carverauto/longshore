// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Kernel ABI for io_uring, plus the libc symbols this crate needs.
//!
//! No `libc` crate: `std` already links the C library, so the symbols are
//! declared here directly. Values are transcribed from the uapi header and are
//! identical on x86-64 and AArch64 (io_uring syscalls use the unified numbers).

#![allow(non_camel_case_types)]

use core::ffi::{c_int, c_long, c_uint, c_void};

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

// Opcodes.
pub const IORING_OP_NOP: u8 = 0;
pub const IORING_OP_TIMEOUT: u8 = 11;
pub const IORING_OP_ACCEPT: u8 = 13;
pub const IORING_OP_ASYNC_CANCEL: u8 = 14;
pub const IORING_OP_CONNECT: u8 = 16;
pub const IORING_OP_OPENAT: u8 = 18;
pub const IORING_OP_CLOSE: u8 = 19;
pub const IORING_OP_READ: u8 = 22;
pub const AT_FDCWD: i32 = -100;
pub const O_RDONLY_CLOEXEC: u32 = 0o2000000;
pub const IORING_OP_SEND: u8 = 26;
pub const IORING_OP_RECV: u8 = 27;
pub const IORING_OP_SOCKET: u8 = 45; // 5.19
pub const IORING_OP_SEND_ZC: u8 = 47; // 6.0

pub const IOSQE_FIXED_FILE: u8 = 1 << 0;

// sqe.ioprio flags for send/recv.
pub const IORING_RECVSEND_FIXED_BUF: u16 = 1 << 2; // 6.0
pub const IORING_SEND_ZC_REPORT_USAGE: u16 = 1 << 3; // 6.2

pub const IORING_CQE_F_MORE: u32 = 1 << 1;
pub const IORING_CQE_F_NOTIF: u32 = 1 << 3;
/// Set in `cqe.res` of a notification CQE when the kernel copied the data
/// instead of sending it zero-copy (requires `IORING_SEND_ZC_REPORT_USAGE`).
pub const IORING_NOTIF_USAGE_ZC_COPIED: u32 = 1 << 31;

pub const MSG_WAITALL: u32 = 0x100;
pub const MSG_NOSIGNAL: u32 = 0x4000;
pub const MSG_DONTWAIT: u32 = 0x40;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;
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

/// Storage for a `sockaddr_in` or a `sockaddr_in6`, which share their first
/// two fields. `len` is how much of it the kernel is told to read.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct sockaddr_any {
    pub family: u16,
    /// Network byte order.
    pub port: [u8; 2],
    /// `sockaddr_in`: the address, then padding. `sockaddr_in6`: flow
    /// information, the address, then the scope id.
    pub rest: [u8; 24],
}

pub const SOCKADDR_IN_LEN: u32 = 16;
pub const SOCKADDR_IN6_LEN: u32 = 28;

impl sockaddr_any {
    /// The kernel's form of `addr`, and its length.
    pub fn from(addr: std::net::SocketAddr) -> (sockaddr_any, u32) {
        let mut s = sockaddr_any {
            port: addr.port().to_be_bytes(),
            ..Default::default()
        };
        match addr {
            std::net::SocketAddr::V4(a) => {
                s.family = AF_INET;
                s.rest[..4].copy_from_slice(&a.ip().octets());
                (s, SOCKADDR_IN_LEN)
            }
            std::net::SocketAddr::V6(a) => {
                s.family = AF_INET6;
                s.rest[..4].copy_from_slice(&a.flowinfo().to_ne_bytes());
                s.rest[4..20].copy_from_slice(&a.ip().octets());
                s.rest[20..24].copy_from_slice(&a.scope_id().to_ne_bytes());
                (s, SOCKADDR_IN6_LEN)
            }
        }
    }
}

/// `struct __kernel_timespec`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct timespec {
    pub sec: i64,
    pub nsec: i64,
}

const _: () = {
    assert!(core::mem::size_of::<io_uring_sqe>() == 64);
    assert!(core::mem::size_of::<io_uring_cqe>() == 16);
    assert!(core::mem::size_of::<io_uring_params>() == 120);
    assert!(core::mem::size_of::<sockaddr_any>() == SOCKADDR_IN6_LEN as usize);
};

pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const MAP_SHARED: c_int = 0x01;
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
    pub fn clock_gettime(clk: c_int, ts: *mut timespec) -> c_int;
}
