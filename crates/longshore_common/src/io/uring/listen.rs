// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A listening TCP socket with options the standard library does not
//! offer before the bind: `SO_REUSEPORT` and the backlog.

use super::sockopts::set_int;
use crate::io::sys::*;
use crate::io::ListenOpts;
use core::ffi::c_int;
use std::io;
use std::net::SocketAddr;

/// A descriptor bound to `addr` and listening; the caller closes it.
pub(super) fn open(addr: SocketAddr, opts: ListenOpts) -> io::Result<c_int> {
    let (sa, len) = sockaddr_any::from(addr);
    let kind = (SOCK_STREAM | SOCK_CLOEXEC) as c_int;
    let fd = unsafe { socket(c_int::from(sa.family), kind, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let listening = (|| {
        set_int(fd, SOL_SOCKET, SO_REUSEADDR, 1).map_err(io::Error::from_raw_os_error)?;
        if opts.reuse_port {
            set_int(fd, SOL_SOCKET, SO_REUSEPORT, 1).map_err(io::Error::from_raw_os_error)?;
        }
        if unsafe { bind(fd, (&raw const sa).cast(), len) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { listen(fd, opts.backlog.min(65_535).cast_signed()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    })();
    if listening.is_err() {
        let _: i32 = unsafe { close(fd) };
    }
    listening
}

/// The address `fd` is bound to.
pub(super) fn bound(fd: c_int) -> io::Result<SocketAddr> {
    let mut sa = sockaddr_any::default();
    let mut len = size_of::<sockaddr_any>() as u32;
    if unsafe { getsockname(fd, (&raw mut sa).cast(), &mut len) } < 0 {
        return Err(io::Error::last_os_error());
    }
    sa.to_socket_addr()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
}
