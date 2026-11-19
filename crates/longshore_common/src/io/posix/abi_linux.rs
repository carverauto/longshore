// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Hand-declared Linux system calls, structs, and constants for `io::posix`.

#![cfg(target_os = "linux")]
#![allow(non_camel_case_types, dead_code)]

use core::ffi::{c_char, c_int, c_long, c_void};
use core::mem::{align_of, offset_of, size_of};

pub const AF_INET: c_int = 2;
pub const AF_INET6: c_int = 10;
pub const SOCK_STREAM: c_int = 1;
pub const SOCK_DGRAM: c_int = 2;
pub const SOCK_NONBLOCK: c_int = 0o4000;
pub const SOCK_CLOEXEC: c_int = 0o200_0000;

pub const SOL_SOCKET: c_int = 1;
pub const SO_REUSEADDR: c_int = 2;
pub const SO_ERROR: c_int = 4;

pub const MSG_DONTWAIT: c_int = 0x40;
pub const MSG_NOSIGNAL: c_int = 0x4000;
pub const MSG_SEND_FLAGS: c_int = MSG_NOSIGNAL | MSG_DONTWAIT;

pub const O_RDONLY: c_int = 0;
pub const O_WRONLY: c_int = 1;
pub const O_RDWR: c_int = 2;
pub const O_CREAT: c_int = 0o100;
pub const O_EXCL: c_int = 0o200;
pub const O_TRUNC: c_int = 0o1000;
pub const O_CLOEXEC: c_int = 0o200_0000;
#[cfg(target_arch = "x86_64")]
pub const O_DIRECT: c_int = 0o0040000;
#[cfg(target_arch = "x86_64")]
pub const O_DIRECTORY: c_int = 0o0200000;
#[cfg(target_arch = "aarch64")]
pub const O_DIRECTORY: c_int = 0o0040000;
#[cfg(target_arch = "aarch64")]
pub const O_DIRECT: c_int = 0o0200000;

pub const AT_FDCWD: c_int = -100;
pub const FALLOC_FL_KEEP_SIZE: c_int = 0x01;
pub const FALLOC_FL_PUNCH_HOLE: c_int = 0x02;
pub const STATX_BASIC_STATS: u32 = 0x07ff;
#[cfg(target_arch = "x86_64")]
pub const SYS_STATX: c_long = 332;
#[cfg(target_arch = "aarch64")]
pub const SYS_STATX: c_long = 291;

pub const F_DUPFD_CLOEXEC: c_int = 1030;

pub const EPOLL_CLOEXEC: c_int = 0o200_0000;
pub const EPOLL_CTL_ADD: c_int = 1;
pub const EPOLL_CTL_DEL: c_int = 2;
pub const EPOLL_CTL_MOD: c_int = 3;

pub const EPOLLIN: u32 = 0x1;
pub const EPOLLOUT: u32 = 0x4;
pub const EPOLLERR: u32 = 0x8;
pub const EPOLLHUP: u32 = 0x10;
pub const EPOLLRDHUP: u32 = 0x2000;

pub const EFD_CLOEXEC: c_int = 0o200_0000;
pub const EFD_NONBLOCK: c_int = 0o4000;

pub const CLOCK_REALTIME: c_int = 0;
pub const CLOCK_MONOTONIC: c_int = 1;

pub const OS_EINTR: c_int = 4;
pub const OS_EAGAIN: c_int = 11;
pub const OS_EWOULDBLOCK: c_int = 11;
pub const OS_ENOTCONN: c_int = 107;
pub const OS_EINPROGRESS: c_int = 115;

#[cfg(target_arch = "x86_64")]
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct epoll_event {
    pub events: u32,
    pub data: u64,
}

#[cfg(not(target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct epoll_event {
    pub events: u32,
    pub data: u64,
}

impl epoll_event {
    #[inline]
    pub fn events_bits(self) -> u32 {
        self.events
    }

    #[inline]
    pub fn data_u64(self) -> u64 {
        self.data
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct sockaddr_in {
    pub sin_family: u16,
    pub sin_port: [u8; 2],
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct sockaddr_in6 {
    pub sin6_family: u16,
    pub sin6_port: [u8; 2],
    pub sin6_flowinfo: u32,
    pub sin6_addr: [u8; 16],
    pub sin6_scope_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct sockaddr_storage {
    pub ss_family: u16,
    pub _pad: [u8; 126],
}

impl Default for sockaddr_storage {
    fn default() -> Self {
        Self {
            ss_family: 0,
            _pad: [0; 126],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

const _: () = {
    #[cfg(target_arch = "x86_64")]
    {
        assert!(size_of::<epoll_event>() == 12);
        assert!(align_of::<epoll_event>() == 1);
        assert!(offset_of!(epoll_event, events) == 0);
        assert!(offset_of!(epoll_event, data) == 4);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        assert!(size_of::<epoll_event>() == 16);
        assert!(align_of::<epoll_event>() == 8);
        assert!(offset_of!(epoll_event, events) == 0);
        assert!(offset_of!(epoll_event, data) == 8);
    }
    assert!(size_of::<sockaddr_in>() == 16);
    assert!(size_of::<sockaddr_in6>() == 28);
    assert!(size_of::<timespec>() == 16);
};

extern "C" {
    pub fn __errno_location() -> *mut c_int;
    pub fn epoll_create1(flags: c_int) -> c_int;
    pub fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut epoll_event) -> c_int;
    pub fn epoll_wait(
        epfd: c_int,
        events: *mut epoll_event,
        maxevents: c_int,
        timeout: c_int,
    ) -> c_int;
    pub fn eventfd(initval: u32, flags: c_int) -> c_int;
    pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    pub fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
    pub fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn listen(fd: c_int, backlog: c_int) -> c_int;
    pub fn getsockname(fd: c_int, addr: *mut c_void, len: *mut u32) -> c_int;
    pub fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn accept4(fd: c_int, addr: *mut c_void, len: *mut u32, flags: c_int) -> c_int;
    pub fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
    pub fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    pub fn close(fd: c_int) -> c_int;
    pub fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: u32) -> c_int;
    pub fn getsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        val: *mut c_void,
        len: *mut u32,
    ) -> c_int;
    pub fn getpeername(fd: c_int, addr: *mut c_void, len: *mut u32) -> c_int;
    pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    pub fn open(path: *const c_char, oflag: c_int, ...) -> c_int;
    pub fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...) -> c_int;
    pub fn pread(fd: c_int, buf: *mut c_void, count: usize, offset: i64) -> isize;
    pub fn pwrite(fd: c_int, buf: *const c_void, count: usize, offset: i64) -> isize;
    pub fn fsync(fd: c_int) -> c_int;
    pub fn fdatasync(fd: c_int) -> c_int;
    pub fn fallocate(fd: c_int, mode: c_int, offset: i64, len: i64) -> c_int;
    pub fn renameat(
        olddirfd: c_int,
        oldpath: *const c_char,
        newdirfd: c_int,
        newpath: *const c_char,
    ) -> c_int;
    pub fn unlinkat(dirfd: c_int, pathname: *const c_char, flags: c_int) -> c_int;
    pub fn mkdirat(dirfd: c_int, pathname: *const c_char, mode: u32) -> c_int;
    pub fn syscall(num: c_long, ...) -> c_long;
    pub fn clock_gettime(clock_id: c_int, tp: *mut timespec) -> c_int;
}

#[inline]
pub fn os_errno() -> c_int {
    unsafe { *__errno_location() }
}
