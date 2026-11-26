// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Socket constants, address storage, and socket syscalls for `io::sys`.

use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};

/// Wait until the whole length is there.
pub const MSG_WAITALL: u32 = 0x100;
/// No `SIGPIPE` when the peer has closed.
pub const MSG_NOSIGNAL: u32 = 0x4000;
/// Linux `MSG_EOR`: the send ends a buffer of the kernel's.
pub const MSG_EOR: u32 = 0x80;
/// Never wait.
pub const MSG_DONTWAIT: u32 = 0x40;
/// Shut down both directions.
pub const SHUT_RDWR: u32 = 2;

/// Unix-domain addresses.
pub const AF_UNIX: u16 = 1;
/// IPv4 addresses.
pub const AF_INET: u16 = 2;
/// IPv6 addresses.
pub const AF_INET6: u16 = 10;
/// A byte stream, such as TCP.
pub const SOCK_STREAM: u64 = 1;
/// Datagrams, such as UDP.
pub const SOCK_DGRAM: u64 = 2;
/// Close the descriptor on `exec`.
pub const SOCK_CLOEXEC: u64 = 0o2000000;

/// Option level of the socket itself.
pub const SOL_SOCKET: c_int = 1;
/// Probe an idle connection.
pub const SO_KEEPALIVE: c_int = 9;
/// Bind an address that old connections still hold.
pub const SO_REUSEADDR: c_int = 2;
/// Let several sockets bind one address.
pub const SO_REUSEPORT: c_int = 15;
/// Seconds of silence before the first keepalive probe.
pub const TCP_KEEPIDLE: c_int = 4;
/// Seconds between keepalive probes.
pub const TCP_KEEPINTVL: c_int = 5;
/// Unanswered keepalive probes before the connection is given up.
pub const TCP_KEEPCNT: c_int = 6;
/// Option level of TCP.
pub const IPPROTO_TCP: c_int = 6;
/// Send small segments at once.
pub const TCP_NODELAY: c_int = 1;
/// The maximum segment size.
pub const TCP_MAXSEG: c_int = 2;
/// Option level of IPv4.
pub const IPPROTO_IP: c_int = 0;
/// The path's MTU, over IPv4.
pub const IP_MTU: c_int = 14;
/// Option level of IPv6.
pub const IPPROTO_IPV6: c_int = 41;
/// The path's MTU, over IPv6.
pub const IPV6_MTU: c_int = 24;

/// Size of a `sockaddr_in`.
pub const SOCKADDR_IN_LEN: u32 = 16;
/// Size of a `sockaddr_in6`.
pub const SOCKADDR_IN6_LEN: u32 = 28;

/// A Unix-domain address: `struct sockaddr_un`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sockaddr_un {
    /// [`AF_UNIX`].
    pub sun_family: u16,
    /// The path, NUL-terminated.
    pub sun_path: [u8; 108],
}

/// Storage large enough for `sockaddr_in`, `sockaddr_in6`, or `sockaddr_un`.
/// `len` tells the kernel how many bytes to read.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sockaddr_any {
    /// The address family, such as [`AF_INET`].
    pub family: u16,
    /// What follows the family, laid out as the family has it.
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

    /// The address this holds, if it is an IPv4 or IPv6 one.
    pub fn to_socket_addr(&self) -> Option<std::net::SocketAddr> {
        use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
        let r = &self.rest;
        let port = u16::from_be_bytes([r[0], r[1]]);
        if self.family == AF_INET {
            let ip = Ipv4Addr::new(r[2], r[3], r[4], r[5]);
            Some(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        } else if self.family == AF_INET6 {
            let flow = u32::from_ne_bytes([r[2], r[3], r[4], r[5]]);
            let ip: [u8; 16] = r[6..22].try_into().ok()?;
            let scope = u32::from_ne_bytes([r[22], r[23], r[24], r[25]]);
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(ip),
                port,
                flow,
                scope,
            )))
        } else {
            None
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
    /// `socket(2)`: open a socket.
    pub fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    /// `bind(2)`: give a socket its address.
    pub fn bind(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    /// `listen(2)`: have a socket take connections.
    pub fn listen(fd: c_int, backlog: c_int) -> c_int;
    /// `getsockname(2)`: read the address a socket is bound to.
    pub fn getsockname(fd: c_int, addr: *mut c_void, len: *mut u32) -> c_int;
    /// `setsockopt(2)`: set a socket option.
    pub fn setsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *const c_void,
        len: u32,
    ) -> c_int;
    /// `getsockopt(2)`: read a socket option.
    pub fn getsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *mut c_void,
        len: *mut u32,
    ) -> c_int;
}
