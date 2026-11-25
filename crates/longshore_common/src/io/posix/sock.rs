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
use core::mem::{offset_of, size_of};
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

/// The address of the peer of connected socket `fd`, if it has an IP one.
pub(super) fn peer_of(fd: c_int) -> Option<SocketAddr> {
    let mut peer = sockaddr_storage::default();
    let mut len = size_of::<sockaddr_storage>() as u32;
    let r = unsafe { getpeername(fd, (&mut peer as *mut sockaddr_storage).cast(), &mut len) };
    if r != 0 {
        return None;
    }
    sockaddr_to(&peer).ok()
}

pub(super) fn sockaddr_to(storage: &sockaddr_storage) -> io::Result<SocketAddr> {
    let family = c_int::from(storage.ss_family);
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
        let _: i32 = unsafe { close(fd) };
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
        if let Err(e) = set_accepted_socket_flags(fd) {
            let _: i32 = unsafe { close(fd) };
            return Err(e);
        }
        return Ok(fd);
    }
}

pub(super) fn sockaddr_un_from(nul_terminated: &[u8]) -> Result<(sockaddr_storage, u32), c_int> {
    let max_path = size_of::<sockaddr_un>() - offset_of!(sockaddr_un, sun_path);
    if nul_terminated.len() > max_path {
        return Err(OS_ENAMETOOLONG);
    }
    let mut sun_path = [0u8; 108];
    sun_path[..nul_terminated.len()].copy_from_slice(nul_terminated);
    let len = (offset_of!(sockaddr_un, sun_path) + nul_terminated.len()) as u32;
    let sa = sockaddr_un {
        #[cfg(target_os = "macos")]
        sun_len: len as u8,
        #[cfg(target_os = "macos")]
        sun_family: AF_UNIX as u8,
        #[cfg(target_os = "linux")]
        sun_family: AF_UNIX as u16,
        #[cfg(target_os = "macos")]
        sun_path: {
            let mut p = [0u8; 104];
            p[..nul_terminated.len()].copy_from_slice(&sun_path[..nul_terminated.len()]);
            p
        },
        #[cfg(target_os = "linux")]
        sun_path,
    };
    let mut storage = sockaddr_storage::default();
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&sa as *const sockaddr_un).cast::<u8>(),
            (&mut storage as *mut sockaddr_storage).cast::<u8>(),
            size_of::<sockaddr_un>(),
        );
    }
    Ok((storage, len))
}

/// Set one integer socket option; the errno on failure.
fn set_int(fd: c_int, level: c_int, name: c_int, value: c_int) -> Result<(), c_int> {
    let len = size_of::<c_int>() as u32;
    let r = unsafe { setsockopt(fd, level, name, (&value as *const c_int).cast(), len) };
    if r < 0 {
        return Err(os_errno());
    }
    Ok(())
}

pub(super) fn apply_tcp_options(
    fd: c_int,
    no_delay: bool,
    keep_alive: bool,
    timing: Option<crate::io::KeepAlive>,
) -> Result<(), c_int> {
    if let (true, Some(t)) = (keep_alive, timing) {
        set_int(fd, IPPROTO_TCP, TCP_KEEPIDLE, t.idle_secs.cast_signed())?;
        set_int(
            fd,
            IPPROTO_TCP,
            TCP_KEEPINTVL,
            t.interval_secs.cast_signed(),
        )?;
        set_int(fd, IPPROTO_TCP, TCP_KEEPCNT, t.count.cast_signed())?;
    }
    let on: c_int = 1;
    let len = size_of::<c_int>() as u32;
    if no_delay {
        let r = unsafe {
            setsockopt(
                fd,
                IPPROTO_TCP,
                TCP_NODELAY,
                (&on as *const c_int).cast(),
                len,
            )
        };
        if r < 0 {
            return Err(os_errno());
        }
    }
    if keep_alive {
        let r = unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_KEEPALIVE,
                (&on as *const c_int).cast(),
                len,
            )
        };
        if r < 0 {
            return Err(os_errno());
        }
    }
    Ok(())
}

impl Posix {
    /// Sets what an accepted socket does not take from its listener: Linux
    /// hands the keepalive timing down, macOS all of it but the idle time.
    /// On failure the socket is closed and the Linux errno returned, so an
    /// option asked for is never silently missing.
    pub(super) fn after_accept(&self, fd: c_int) -> Result<(), i32> {
        if cfg!(target_os = "macos") {
            if let (true, Some(t)) = (self.keep_alive, self.keep_timing) {
                let idle = t.idle_secs.cast_signed();
                if let Err(e) = set_int(fd, IPPROTO_TCP, TCP_KEEPIDLE, idle) {
                    let _: i32 = unsafe { close(fd) };
                    return Err(super::errno::to_linux_errno(e));
                }
            }
        }
        Ok(())
    }
}

/// `(idle, interval, count)` keepalive timing read from `fd`.
pub(super) fn read_keep_timing(fd: c_int) -> io::Result<(u32, u32, u32)> {
    let read_one = |name: c_int| -> io::Result<u32> {
        let mut val: c_int = 0;
        let mut len = size_of::<c_int>() as u32;
        let p = (&mut val as *mut c_int).cast();
        if unsafe { getsockopt(fd, IPPROTO_TCP, name, p, &mut len) } < 0 {
            return Err(io::Error::from_raw_os_error(os_errno()));
        }
        Ok(val.cast_unsigned())
    };
    Ok((
        read_one(TCP_KEEPIDLE)?,
        read_one(TCP_KEEPINTVL)?,
        read_one(TCP_KEEPCNT)?,
    ))
}

pub(super) fn read_tcp_options(fd: c_int) -> io::Result<(bool, bool)> {
    let read_one = |level: c_int, name: c_int| -> io::Result<bool> {
        let mut val: c_int = 0;
        let mut len = size_of::<c_int>() as u32;
        if unsafe { getsockopt(fd, level, name, (&mut val as *mut c_int).cast(), &mut len) } < 0 {
            return Err(io::Error::from_raw_os_error(os_errno()));
        }
        Ok(val != 0)
    };
    Ok((
        read_one(IPPROTO_TCP, TCP_NODELAY)?,
        read_one(SOL_SOCKET, SO_KEEPALIVE)?,
    ))
}

impl Posix {
    pub(super) fn listen_sock(
        &mut self,
        sock: Sock,
        addr: SocketAddr,
        opts: crate::io::ListenOpts,
    ) -> io::Result<SocketAddr> {
        let domain = match addr {
            SocketAddr::V4(_) => AF_INET,
            SocketAddr::V6(_) => AF_INET6,
        };
        let fd = open_socket(domain, SOCK_STREAM).map_err(io::Error::from_raw_os_error)?;
        let on: c_int = 1;
        unsafe {
            let _: i32 = setsockopt(
                fd,
                SOL_SOCKET,
                SO_REUSEADDR,
                (&on as *const c_int).cast(),
                size_of::<c_int>() as u32,
            );
        }
        if opts.reuse_port {
            if let Err(e) = set_int(fd, SOL_SOCKET, SO_REUSEPORT, 1) {
                let _: i32 = unsafe { close(fd) };
                return Err(io::Error::from_raw_os_error(e));
            }
        }
        if self.no_delay || self.keep_alive {
            if let Err(e) = apply_tcp_options(fd, self.no_delay, self.keep_alive, self.keep_timing)
            {
                let _: i32 = unsafe { close(fd) };
                return Err(io::Error::from_raw_os_error(e));
            }
        }
        let (sa, sa_len) = sockaddr_from(addr);
        if unsafe { bind(fd, (&sa as *const sockaddr_storage).cast(), sa_len) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            let _: i32 = unsafe { close(fd) };
            return Err(err);
        }
        if unsafe { listen(fd, opts.backlog.min(65_535).cast_signed()) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            let _: i32 = unsafe { close(fd) };
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
            let _: i32 = unsafe { close(fd) };
            return Err(err);
        }
        let local = match sockaddr_to(&bound) {
            Ok(a) => a,
            Err(e) => {
                let _: i32 = unsafe { close(fd) };
                return Err(e);
            }
        };
        let slot = &mut self.socks[sock.0 as usize];
        if slot.fd >= 0 {
            self.poller.before_close(slot);
            let _: i32 = unsafe { close(slot.fd) };
        }
        slot.reset();
        slot.fd = fd;
        slot.kind = SockKind::Listening;
        Ok(local)
    }

    pub(super) fn listen_unix_sock(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        if path.is_empty() || path.as_bytes().contains(&0) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if path.len() >= 108 {
            return Err(io::Error::from_raw_os_error(OS_ENAMETOOLONG));
        }
        let mut buf = [0u8; 108];
        buf[..path.len()].copy_from_slice(path.as_bytes());
        let (sa, sa_len) =
            sockaddr_un_from(&buf[..path.len() + 1]).map_err(io::Error::from_raw_os_error)?;
        let fd = open_socket(AF_UNIX, SOCK_STREAM).map_err(io::Error::from_raw_os_error)?;
        if unsafe { bind(fd, (&sa as *const sockaddr_storage).cast(), sa_len) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            let _: i32 = unsafe { close(fd) };
            return Err(err);
        }
        if unsafe { listen(fd, 128) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            let _: i32 = unsafe { close(fd) };
            return Err(err);
        }
        let slot = &mut self.socks[sock.0 as usize];
        if slot.fd >= 0 {
            self.poller.before_close(slot);
            let _: i32 = unsafe { close(slot.fd) };
        }
        slot.reset();
        slot.fd = fd;
        slot.kind = SockKind::Listening;
        Ok(())
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
