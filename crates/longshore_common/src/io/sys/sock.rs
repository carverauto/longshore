// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Socket constants, address storage, and socket syscalls for `io::sys`.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};

pub const MSG_WAITALL: u32 = 0x100;
pub const MSG_NOSIGNAL: u32 = 0x4000;
pub const MSG_DONTWAIT: u32 = 0x40;
pub const SHUT_RDWR: u32 = 2;

pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;
pub const SOCK_CLOEXEC: u64 = 0o2000000;

pub const SOL_SOCKET: c_int = 1;
pub const SO_KEEPALIVE: c_int = 9;
pub const IPPROTO_TCP: c_int = 6;
pub const TCP_NODELAY: c_int = 1;
pub const TCP_MAXSEG: c_int = 2;
pub const IPPROTO_IP: c_int = 0;
pub const IP_MTU: c_int = 14;
pub const IPPROTO_IPV6: c_int = 41;
pub const IPV6_MTU: c_int = 24;

pub const SOCKADDR_IN_LEN: u32 = 16;
pub const SOCKADDR_IN6_LEN: u32 = 28;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct sockaddr_un {
    pub sun_family: u16,
    pub sun_path: [u8; 108],
}

/// Storage large enough for `sockaddr_in`, `sockaddr_in6`, or `sockaddr_un`.
/// `len` tells the kernel how many bytes to read.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sockaddr_any {
    pub family: u16,
    pub rest: [u8; 126],
}

impl Default for sockaddr_any {
    fn default() -> Self {
        Self {
            family: 0,
            rest: [0; 126],
        }
    }
}

impl sockaddr_any {
    /// The kernel's form of `addr`, and its length.
    pub fn from(addr: std::net::SocketAddr) -> (sockaddr_any, u32) {
        let mut s = sockaddr_any::default();
        s.rest[..2].copy_from_slice(&addr.port().to_be_bytes());
        match addr {
            std::net::SocketAddr::V4(a) => {
                s.family = AF_INET;
                s.rest[2..6].copy_from_slice(&a.ip().octets());
                (s, SOCKADDR_IN_LEN)
            }
            std::net::SocketAddr::V6(a) => {
                s.family = AF_INET6;
                s.rest[2..6].copy_from_slice(&a.flowinfo().to_ne_bytes());
                s.rest[6..22].copy_from_slice(&a.ip().octets());
                s.rest[22..26].copy_from_slice(&a.scope_id().to_ne_bytes());
                (s, SOCKADDR_IN6_LEN)
            }
        }
    }

    /// The kernel's form of a NUL-terminated Unix-domain socket path, and its length.
    pub fn from_unix(nul_terminated: &[u8]) -> (sockaddr_any, u32) {
        assert!(
            nul_terminated.len() > 1
                && nul_terminated.len() <= 108
                && nul_terminated[nul_terminated.len() - 1] == 0,
            "invalid NUL-terminated Unix path"
        );
        let mut s = sockaddr_any {
            family: AF_UNIX,
            rest: [0; 126],
        };
        s.rest[..nul_terminated.len()].copy_from_slice(nul_terminated);
        (s, 2 + nul_terminated.len() as u32)
    }
}

const _: () = {
    assert!(size_of::<sockaddr_any>() == 128);
    assert!(size_of::<sockaddr_un>() == 110);
    assert!(offset_of!(sockaddr_un, sun_path) == 2);
};

extern "C" {
    pub fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    pub fn setsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *const c_void,
        len: u32,
    ) -> c_int;
    pub fn getsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *mut c_void,
        len: *mut u32,
    ) -> c_int;
}
