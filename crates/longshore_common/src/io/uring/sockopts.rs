// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TCP socket options on the ring backend: setting them on a descriptor,
//! and reading back what the kernel took, for the checks.

use super::*;
use crate::io::EINVAL;
use core::ffi::c_int;

impl UringIo {
    /// Keepalive `(idle, interval, count)` read from the most recent `listen` descriptor.
    pub fn last_listen_keep_timing(&self) -> Option<(u32, u32, u32)> {
        self.last_listen_timing
    }

    /// Keepalive `(idle, interval, count)` read from the most recent
    /// descriptor-created `Connect`.
    pub fn last_connect_keep_timing(&self) -> Option<(u32, u32, u32)> {
        self.last_connect_timing
    }

    /// `(TCP_NODELAY, SO_KEEPALIVE)` read from the most recent `listen` descriptor.
    pub fn last_listen_tcp_options(&self) -> Option<(bool, bool)> {
        self.last_listen_opts
    }

    /// `(TCP_NODELAY, SO_KEEPALIVE)` read from the most recent descriptor-created `Connect`.
    pub fn last_connect_tcp_options(&self) -> Option<(bool, bool)> {
        self.last_connect_opts
    }

    /// Read `(TCP_NODELAY, SO_KEEPALIVE)` directly from fixed-file slot `sock`
    /// using `SOCKET_URING_OP_GETSOCKOPT` (Linux 6.7+). Returns `Ok(None)` on
    /// kernels before 6.7 (`-EINVAL` / `-EOPNOTSUPP`). Requires `in_flight == 0`.
    pub fn probe_tcp_options(&mut self, sock: Sock) -> io::Result<Option<(bool, bool)>> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        assert_eq!(self.in_flight, 0, "probe_tcp_options requires an idle ring");
        let probe_one =
            |ring: &mut Ring, level: c_int, optname: c_int| -> io::Result<Option<bool>> {
                // The command's `level` and `optname` are `__u32` in the uapi.
                let invalid = |_| io::Error::from(io::ErrorKind::InvalidInput);
                let level = u32::try_from(level).map_err(invalid)?;
                let optname = u32::try_from(optname).map_err(invalid)?;
                let mut val: c_int = 0;
                let sqe = io_uring_sqe {
                    opcode: IORING_OP_URING_CMD,
                    flags: IOSQE_FIXED_FILE,
                    fd: fixed_fd(sock.0),
                    off: SOCKET_URING_OP_GETSOCKOPT,
                    addr: u64::from(level) | (u64::from(optname) << 32),
                    file_index: core::mem::size_of::<c_int>() as u32,
                    addr3: (&mut val as *mut c_int) as u64,
                    ..Default::default()
                };
                ring.push(sqe).map_err(|_| io::Error::other("sq full"))?;
                let _: u32 = ring.enter(1)?;
                let cqe = ring.pop().ok_or_else(|| io::Error::other("missing cqe"))?;
                if cqe.res == -EINVAL || cqe.res == -95 {
                    return Ok(None);
                }
                if cqe.res < 0 {
                    return Err(io::Error::from_raw_os_error(-cqe.res));
                }
                Ok(Some(val != 0))
            };
        let Some(nd) = probe_one(&mut self.ring, IPPROTO_TCP, TCP_NODELAY)? else {
            return Ok(None);
        };
        let Some(ka) = probe_one(&mut self.ring, SOL_SOCKET, SO_KEEPALIVE)? else {
            return Ok(None);
        };
        Ok(Some((nd, ka)))
    }
}

/// Set one integer socket option; the errno on failure.
pub(super) fn set_int(fd: c_int, level: c_int, name: c_int, value: c_int) -> Result<(), i32> {
    let len = core::mem::size_of::<c_int>() as u32;
    let r = unsafe { setsockopt(fd, level, name, (&value as *const c_int).cast(), len) };
    if r < 0 {
        return Err(io::Error::last_os_error().raw_os_error().unwrap_or(EINVAL));
    }
    Ok(())
}

pub(super) fn apply_tcp_options(
    fd: c_int,
    no_delay: bool,
    keep_alive: bool,
    timing: Option<crate::io::KeepAlive>,
) -> Result<(), i32> {
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
    let len = core::mem::size_of::<c_int>() as u32;
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
            return Err(io::Error::last_os_error().raw_os_error().unwrap_or(EINVAL));
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
            return Err(io::Error::last_os_error().raw_os_error().unwrap_or(EINVAL));
        }
    }
    Ok(())
}

/// `(idle, interval, count)` keepalive timing read from `fd`; zeros for
/// what cannot be read.
pub(super) fn read_keep_timing(fd: c_int) -> (u32, u32, u32) {
    let read_one = |name: c_int| -> u32 {
        let mut val: c_int = 0;
        let mut len = core::mem::size_of::<c_int>() as u32;
        let p = (&mut val as *mut c_int).cast();
        let r = unsafe { getsockopt(fd, IPPROTO_TCP, name, p, &mut len) };
        if r == 0 {
            val.cast_unsigned()
        } else {
            0
        }
    };
    (
        read_one(TCP_KEEPIDLE),
        read_one(TCP_KEEPINTVL),
        read_one(TCP_KEEPCNT),
    )
}

pub(super) fn read_tcp_options(fd: c_int) -> (bool, bool) {
    let read_one = |level: c_int, name: c_int| -> bool {
        let mut val: c_int = 0;
        let mut len = core::mem::size_of::<c_int>() as u32;
        let r = unsafe { getsockopt(fd, level, name, (&mut val as *mut c_int).cast(), &mut len) };
        r == 0 && val != 0
    };
    (
        read_one(IPPROTO_TCP, TCP_NODELAY),
        read_one(SOL_SOCKET, SO_KEEPALIVE),
    )
}
