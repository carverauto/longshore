// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Socket address conversion, non-blocking socket creation, and `listen`.

#[cfg(target_os = "linux")]
use super::abi_linux::*;
#[cfg(target_os = "macos")]
use super::abi_macos::*;
use super::{Posix, SockKind};
use crate::io::Sock;
use core::ffi::c_int;
use core::mem::size_of;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub(super) fn sockaddr_from(addr: SocketAddr) -> (sockaddr_storage, u32) {
    let mut storage = sockaddr_storage::default();
    let len = match addr {
        SocketAddr::V4(v4) => {
            let sa = sockaddr_in {
                #[cfg(target_os = "macos")]
                sin_len: size_of::<sockaddr_in>() as u8,
                #[cfg(target_os = "macos")]
                sin_family: AF_INET as u8,
                #[cfg(target_os = "linux")]
                sin_family: AF_INET as u16,
                sin_port: v4.port().to_be_bytes(),
                sin_addr: v4.ip().octets(),
                sin_zero: [0; 8],
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (&sa as *const sockaddr_in).cast::<u8>(),
                    (&mut storage as *mut sockaddr_storage).cast::<u8>(),
                    size_of::<sockaddr_in>(),
                );
            }
            size_of::<sockaddr_in>() as u32
        }
        SocketAddr::V6(v6) => {
            let sa = sockaddr_in6 {
                #[cfg(target_os = "macos")]
                sin6_len: size_of::<sockaddr_in6>() as u8,
                #[cfg(target_os = "macos")]
                sin6_family: AF_INET6 as u8,
                #[cfg(target_os = "linux")]
                sin6_family: AF_INET6 as u16,
                sin6_port: v6.port().to_be_bytes(),
                sin6_flowinfo: v6.flowinfo().to_be(),
                sin6_addr: v6.ip().octets(),
                sin6_scope_id: v6.scope_id(),
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (&sa as *const sockaddr_in6).cast::<u8>(),
                    (&mut storage as *mut sockaddr_storage).cast::<u8>(),
                    size_of::<sockaddr_in6>(),
                );
            }
            size_of::<sockaddr_in6>() as u32
        }
    };
    (storage, len)
}

pub(super) fn sockaddr_to(storage: &sockaddr_storage) -> io::Result<SocketAddr> {
    let family = storage.ss_family as c_int;
    if family == AF_INET {
        let mut sa = sockaddr_in::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                (storage as *const sockaddr_storage).cast::<u8>(),
                (&mut sa as *mut sockaddr_in).cast::<u8>(),
                size_of::<sockaddr_in>(),
            );
        }
        Ok(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::from(sa.sin_addr),
            u16::from_be_bytes(sa.sin_port),
        )))
    } else if family == AF_INET6 {
        let mut sa = sockaddr_in6::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                (storage as *const sockaddr_storage).cast::<u8>(),
                (&mut sa as *mut sockaddr_in6).cast::<u8>(),
                size_of::<sockaddr_in6>(),
            );
        }
        Ok(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::from(sa.sin6_addr),
            u16::from_be_bytes(sa.sin6_port),
            u32::from_be(sa.sin6_flowinfo),
            sa.sin6_scope_id,
        )))
    } else {
        Err(io::Error::from(io::ErrorKind::InvalidInput))
    }
}

pub(super) fn open_socket(domain: c_int, kind: c_int) -> Result<c_int, c_int> {
    #[cfg(target_os = "linux")]
    let fd = unsafe { socket(domain, kind | SOCK_NONBLOCK | SOCK_CLOEXEC, 0) };
    #[cfg(target_os = "macos")]
    let fd = unsafe { socket(domain, kind, 0) };
    if fd < 0 {
        return Err(os_errno());
    }
    #[cfg(target_os = "macos")]
    if !set_socket_flags(fd) {
        let e = os_errno();
        unsafe { close(fd) };
        return Err(e);
    }
    Ok(fd)
}

pub(super) fn accept_socket(listener_fd: c_int) -> Result<c_int, c_int> {
    loop {
        #[cfg(target_os = "linux")]
        let fd = unsafe {
            accept4(
                listener_fd,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                SOCK_NONBLOCK | SOCK_CLOEXEC,
            )
        };
        #[cfg(target_os = "macos")]
        let fd = unsafe { accept(listener_fd, core::ptr::null_mut(), core::ptr::null_mut()) };
        if fd < 0 {
            let e = os_errno();
            if e == OS_EINTR {
                continue;
            }
            return Err(e);
        }
        #[cfg(target_os = "macos")]
        if !set_socket_flags(fd) {
            let e = os_errno();
            unsafe { close(fd) };
            return Err(e);
        }
        return Ok(fd);
    }
}

impl Posix {
    pub(super) fn listen_sock(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        let domain = match addr {
            SocketAddr::V4(_) => AF_INET,
            SocketAddr::V6(_) => AF_INET6,
        };
        let fd = open_socket(domain, SOCK_STREAM).map_err(io::Error::from_raw_os_error)?;
        let on: c_int = 1;
        unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_REUSEADDR,
                (&on as *const c_int).cast(),
                size_of::<c_int>() as u32,
            );
        }
        let (sa, sa_len) = sockaddr_from(addr);
        if unsafe { bind(fd, (&sa as *const sockaddr_storage).cast(), sa_len) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            unsafe { close(fd) };
            return Err(err);
        }
        if unsafe { listen(fd, 128) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            unsafe { close(fd) };
            return Err(err);
        }
        let mut bound = sockaddr_storage::default();
        let mut bound_len = size_of::<sockaddr_storage>() as u32;
        if unsafe {
            getsockname(
                fd,
                (&mut bound as *mut sockaddr_storage).cast(),
                &mut bound_len,
            )
        } < 0
        {
            let err = io::Error::from_raw_os_error(os_errno());
            unsafe { close(fd) };
            return Err(err);
        }
        let local = match sockaddr_to(&bound) {
            Ok(a) => a,
            Err(e) => {
                unsafe { close(fd) };
                return Err(e);
            }
        };
        let slot = &mut self.socks[sock.0 as usize];
        if slot.fd >= 0 {
            self.poller.before_close(slot);
            unsafe { close(slot.fd) };
        }
        slot.reset();
        slot.fd = fd;
        slot.kind = SockKind::Listening;
        Ok(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v6_flowinfo_roundtrips_in_network_byte_order() {
        let orig = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 8080, 0x12345, 7);
        let (storage, _) = sockaddr_from(SocketAddr::V6(orig));
        let mut sa = sockaddr_in6::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&storage as *const sockaddr_storage).cast::<u8>(),
                (&mut sa as *mut sockaddr_in6).cast::<u8>(),
                size_of::<sockaddr_in6>(),
            );
        }
        assert_eq!(sa.sin6_flowinfo, 0x12345_u32.to_be());
        assert_eq!(sockaddr_to(&storage).unwrap(), SocketAddr::V6(orig));
    }
}
