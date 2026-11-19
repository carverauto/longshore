// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Hand-declared macOS system calls, structs, and constants for `io::posix`.

#![cfg(target_os = "macos")]
#![allow(non_camel_case_types, dead_code)]

use core::ffi::{c_char, c_int, c_void};
use core::mem::{align_of, offset_of, size_of};

pub const AF_INET: c_int = 2;
pub const AF_INET6: c_int = 30;
pub const SOCK_STREAM: c_int = 1;
pub const SOCK_DGRAM: c_int = 2;

pub const SOL_SOCKET: c_int = 0xffff;
pub const SO_REUSEADDR: c_int = 0x0004;
pub const SO_ERROR: c_int = 0x1007;
pub const SO_NOSIGPIPE: c_int = 0x1022;

pub const MSG_DONTWAIT: c_int = 0x80;
pub const MSG_SEND_FLAGS: c_int = MSG_DONTWAIT;

pub const O_RDONLY: c_int = 0x0000;
pub const O_WRONLY: c_int = 0x0001;
pub const O_RDWR: c_int = 0x0002;
pub const O_NONBLOCK: c_int = 0x0004;
pub const O_CREAT: c_int = 0x0200;
pub const O_TRUNC: c_int = 0x0400;
pub const O_EXCL: c_int = 0x0800;
pub const O_DIRECTORY: c_int = 0x0010_0000;
pub const O_CLOEXEC: c_int = 0x0100_0000;

pub const F_SETFD: c_int = 2;
pub const F_GETFL: c_int = 3;
pub const F_SETFL: c_int = 4;
pub const F_PREALLOCATE: c_int = 42;
pub const F_NOCACHE: c_int = 48;
pub const F_FULLFSYNC: c_int = 51;
pub const F_DUPFD_CLOEXEC: c_int = 67;
pub const F_PUNCHHOLE: c_int = 99;
pub const FD_CLOEXEC: c_int = 1;

pub const F_ALLOCATECONTIG: u32 = 0x2;
pub const F_ALLOCATEALL: u32 = 0x4;
pub const F_PEOFPOSMODE: c_int = 3;

pub const EVFILT_READ: i16 = -1;
pub const EVFILT_WRITE: i16 = -2;
pub const EVFILT_USER: i16 = -10;

pub const EV_ADD: u16 = 0x0001;
pub const EV_DELETE: u16 = 0x0002;
pub const EV_ENABLE: u16 = 0x0004;
pub const EV_ONESHOT: u16 = 0x0010;
pub const EV_CLEAR: u16 = 0x0020;
pub const EV_ERROR: u16 = 0x4000;
pub const EV_EOF: u16 = 0x8000;

pub const NOTE_TRIGGER: u32 = 0x0100_0000;

pub const CLOCK_REALTIME: c_int = 0;
pub const CLOCK_MONOTONIC: c_int = 6;

pub const OS_EINTR: c_int = 4;
pub const OS_EAGAIN: c_int = 35;
pub const OS_EWOULDBLOCK: c_int = 35;
pub const OS_EINPROGRESS: c_int = 36;
pub const OS_ENOTCONN: c_int = 57;

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct kevent {
    pub ident: usize,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: isize,
    pub udata: *mut c_void,
}

impl Default for kevent {
    fn default() -> Self {
        Self {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: core::ptr::null_mut(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct sockaddr_in {
    pub sin_len: u8,
    pub sin_family: u8,
    pub sin_port: [u8; 2],
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct sockaddr_in6 {
    pub sin6_len: u8,
    pub sin6_family: u8,
    pub sin6_port: [u8; 2],
    pub sin6_flowinfo: u32,
    pub sin6_addr: [u8; 16],
    pub sin6_scope_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct sockaddr_storage {
    pub ss_len: u8,
    pub ss_family: u8,
    pub _pad: [u8; 126],
}

impl Default for sockaddr_storage {
    fn default() -> Self {
        Self {
            ss_len: 0,
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

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct fstore_t {
    pub fst_flags: u32,
    pub fst_posmode: c_int,
    pub fst_offset: i64,
    pub fst_length: i64,
    pub fst_bytesalloc: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct fpunchhole_t {
    pub fp_flags: u32,
    pub reserved: u32,
    pub fp_offset: i64,
    pub fp_length: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct stat {
    pub st_dev: i32,
    pub st_mode: u16,
    pub st_nlink: u16,
    pub st_ino: u64,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: i32,
    pub _pad0: i32,
    pub st_atime: timespec,
    pub st_mtime: timespec,
    pub st_ctime: timespec,
    pub st_birthtime: timespec,
    pub st_size: i64,
    pub st_blocks: i64,
    pub st_blksize: i32,
    pub st_flags: u32,
    pub st_gen: u32,
    pub st_lspare: i32,
    pub st_qspare: [i64; 2],
}

const _: () = {
    assert!(size_of::<kevent>() == 32);
    assert!(align_of::<kevent>() == 4);
    assert!(offset_of!(kevent, ident) == 0);
    assert!(offset_of!(kevent, filter) == 8);
    assert!(offset_of!(kevent, flags) == 10);
    assert!(offset_of!(kevent, fflags) == 12);
    assert!(offset_of!(kevent, data) == 16);
    assert!(offset_of!(kevent, udata) == 24);
    assert!(size_of::<sockaddr_in>() == 16);
    assert!(size_of::<sockaddr_in6>() == 28);
    assert!(size_of::<timespec>() == 16);
    assert!(size_of::<fstore_t>() == 32);
    assert!(size_of::<fpunchhole_t>() == 24);
    assert!(size_of::<stat>() == 144);
    assert!(offset_of!(stat, st_mode) == 4);
    assert!(offset_of!(stat, st_nlink) == 6);
    assert!(offset_of!(stat, st_ino) == 8);
    assert!(offset_of!(stat, st_uid) == 16);
    assert!(offset_of!(stat, st_gid) == 20);
    assert!(offset_of!(stat, st_size) == 96);
    assert!(offset_of!(stat, st_blocks) == 104);
    assert!(offset_of!(stat, st_blksize) == 112);
};

extern "C" {
    pub fn __error() -> *mut c_int;
    pub fn kqueue() -> c_int;
    pub fn kevent(
        kq: c_int,
        changelist: *const kevent,
        nchanges: c_int,
        eventlist: *mut kevent,
        nevents: c_int,
        timeout: *const timespec,
    ) -> c_int;
    pub fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
    pub fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn listen(fd: c_int, backlog: c_int) -> c_int;
    pub fn getsockname(fd: c_int, addr: *mut c_void, len: *mut u32) -> c_int;
    pub fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub fn accept(fd: c_int, addr: *mut c_void, len: *mut u32) -> c_int;
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
    pub fn pread(fd: c_int, buf: *mut c_void, count: usize, offset: i64) -> isize;
    pub fn pwrite(fd: c_int, buf: *const c_void, count: usize, offset: i64) -> isize;
    pub fn ftruncate(fd: c_int, length: i64) -> c_int;
    pub fn rename(old: *const c_char, new: *const c_char) -> c_int;
    pub fn unlink(path: *const c_char) -> c_int;
    pub fn mkdir(path: *const c_char, mode: u16) -> c_int;
    #[cfg_attr(target_arch = "x86_64", link_name = "stat$INODE64")]
    pub fn stat(path: *const c_char, buf: *mut stat) -> c_int;
    #[cfg_attr(target_arch = "x86_64", link_name = "fstat$INODE64")]
    pub fn fstat(fd: c_int, buf: *mut stat) -> c_int;
    pub fn clock_gettime(clock_id: c_int, tp: *mut timespec) -> c_int;
}

#[inline]
pub fn os_errno() -> c_int {
    unsafe { *__error() }
}

pub fn set_socket_flags(fd: c_int) -> bool {
    unsafe {
        if fcntl(fd, F_SETFD, FD_CLOEXEC) < 0 {
            return false;
        }
        let fl = fcntl(fd, F_GETFL, 0);
        if fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) < 0 {
            return false;
        }
        let on: c_int = 1;
        setsockopt(
            fd,
            SOL_SOCKET,
            SO_NOSIGPIPE,
            (&on as *const c_int).cast(),
            size_of::<c_int>() as u32,
        ) == 0
    }
}
