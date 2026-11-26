// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Non-blocking socket and blocking file operations for [`Posix`].

#[cfg(target_os = "linux")]
use super::abi_linux::*;
#[cfg(target_os = "macos")]
use super::abi_macos::*;
use super::errno::{self, to_linux_errno};
use super::sock::{accept_socket, apply_tcp_options, open_socket, sockaddr_from};
use super::{Posix, SockKind};
use crate::io::{Limits, Op, Sock};
use core::ffi::{c_int, c_void};
use core::mem::size_of;
use std::net::SocketAddr;

impl Posix {
    /// Shuts one or both directions of a connected stream down.
    fn attempt_shutdown(&self, sock: Sock, how: crate::io::Shut) -> i32 {
        let s = &self.socks[sock.0 as usize];
        if s.fd < 0 || s.kind != SockKind::Open {
            return -crate::io::ENOTCONN;
        }
        let r = unsafe { shutdown(s.fd, how.raw().cast_signed()) };
        if r == 0 {
            0
        } else {
            -to_linux_errno(os_errno())
        }
    }

    /// Closes a socket slot, shutting a connected stream down first.
    fn attempt_close(&mut self, sock: Sock) -> i32 {
        let s = &mut self.socks[sock.0 as usize];
        if s.fd >= 0 {
            if s.kind == SockKind::Open {
                let _: i32 = unsafe { shutdown(s.fd, SHUT_RDWR) };
            }
            self.poller.before_close(s);
            let _: i32 = unsafe { close(s.fd) };
            s.reset();
        }
        0
    }

    pub(super) fn attempt_op(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        match *op {
            Op::Timeout { .. } | Op::AwaitWake | Op::Cancel { .. } => {
                unreachable!("resolved by Engine")
            }
            Op::Connect { sock, addr } => self.attempt_connect(sock, addr),
            Op::ConnectUdp { sock, addr } => self.attempt_connect_udp(sock, addr),
            Op::Accept { listener, sock } => self.attempt_accept(listener, sock),
            // A zero-copy send is copied here, and so reports once.
            Op::Send {
                sock,
                buf,
                off,
                len,
                ..
            }
            | Op::SendZc {
                sock,
                buf,
                off,
                len,
            } => self.attempt_send(sock, pool, limits.range(buf, off, len)),
            Op::Recv {
                sock,
                buf,
                off,
                len,
            } => self.attempt_recv(sock, pool, limits.range(buf, off, len), false),
            Op::RecvNow {
                sock,
                buf,
                off,
                len,
            } => self.attempt_recv(sock, pool, limits.range(buf, off, len), true),
            Op::Shutdown { sock, how } => Some(self.attempt_shutdown(sock, how)),
            Op::Close { sock } => Some(self.attempt_close(sock)),
            Op::ReadFile {
                file,
                buf,
                off,
                len,
            } => self.attempt_read_file(file.0 as usize, pool, limits.range(buf, off, len)),
            Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileWriteSpans { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => self.attempt_file_op(op, pool, limits),
            Op::ConnectUnix { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::ArmedRecvZc { .. } => {
                unreachable!("handled above")
            }
        }
    }

    pub(super) fn finish_connecting(&mut self, idx: usize) -> Option<i32> {
        let fd = self.socks[idx].fd;
        let mut err: c_int = 0;
        let mut elen = size_of::<c_int>() as u32;
        if unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                SO_ERROR,
                (&mut err as *mut c_int).cast(),
                &mut elen,
            )
        } < 0
        {
            err = os_errno();
        }
        if err != 0 {
            self.poller.before_close(&mut self.socks[idx]);
            let _: i32 = unsafe { close(fd) };
            self.socks[idx].reset();
            return Some(to_linux_errno(err));
        }
        let mut peer = sockaddr_storage::default();
        let mut plen = size_of::<sockaddr_storage>() as u32;
        if unsafe { getpeername(fd, (&mut peer as *mut sockaddr_storage).cast(), &mut plen) } == 0 {
            self.socks[idx].kind = SockKind::Open;
            return Some(0);
        }
        let e = os_errno();
        if e == OS_ENOTCONN {
            self.socks[idx].write_ready = false;
            self.poller.arm_write(idx, &mut self.socks[idx]);
            return None;
        }
        self.poller.before_close(&mut self.socks[idx]);
        let _: i32 = unsafe { close(fd) };
        self.socks[idx].reset();
        Some(to_linux_errno(e))
    }

    fn attempt_connect(&mut self, sock: Sock, addr: SocketAddr) -> Option<i32> {
        let idx = sock.0 as usize;
        if self.socks[idx].kind == SockKind::Connecting {
            return self.finish_connecting(idx);
        }

        let domain = match addr {
            SocketAddr::V4(_) => AF_INET,
            SocketAddr::V6(_) => AF_INET6,
        };
        let fd = match open_socket(domain, SOCK_STREAM) {
            Ok(fd) => fd,
            Err(e) => return Some(to_linux_errno(e)),
        };
        if self.no_delay || self.keep_alive {
            if let Err(e) = apply_tcp_options(fd, self.no_delay, self.keep_alive, self.keep_timing)
            {
                let _: i32 = unsafe { close(fd) };
                return Some(to_linux_errno(e));
            }
        }
        let (sa, sa_len) = sockaddr_from(addr);
        if unsafe { connect(fd, (&sa as *const sockaddr_storage).cast(), sa_len) } == 0 {
            self.socks[idx].reset();
            self.socks[idx].fd = fd;
            self.socks[idx].kind = SockKind::Open;
            return Some(0);
        }
        let e = os_errno();
        if e == OS_EINPROGRESS || e == OS_EINTR {
            self.socks[idx].reset();
            self.socks[idx].fd = fd;
            self.socks[idx].kind = SockKind::Connecting;
            self.poller.arm_write(idx, &mut self.socks[idx]);
            return None;
        }
        let _: i32 = unsafe { close(fd) };
        Some(to_linux_errno(e))
    }

    fn attempt_connect_udp(&mut self, sock: Sock, addr: SocketAddr) -> Option<i32> {
        let idx = sock.0 as usize;
        let domain = match addr {
            SocketAddr::V4(_) => AF_INET,
            SocketAddr::V6(_) => AF_INET6,
        };
        let fd = match open_socket(domain, SOCK_DGRAM) {
            Ok(fd) => fd,
            Err(e) => return Some(to_linux_errno(e)),
        };
        let (sa, sa_len) = sockaddr_from(addr);
        if unsafe { connect(fd, (&sa as *const sockaddr_storage).cast(), sa_len) } < 0 {
            let e = os_errno();
            let _: i32 = unsafe { close(fd) };
            return Some(to_linux_errno(e));
        }
        if self.socks[idx].fd >= 0 {
            self.poller.before_close(&mut self.socks[idx]);
            let _: i32 = unsafe { close(self.socks[idx].fd) };
        }
        self.socks[idx].reset();
        self.socks[idx].fd = fd;
        self.socks[idx].kind = SockKind::Udp;
        Some(0)
    }

    fn attempt_accept(&mut self, listener: Sock, sock: Sock) -> Option<i32> {
        let lidx = listener.0 as usize;
        let lfd = self.socks[lidx].fd;
        if lfd < 0 {
            return Some(-errno::linux::EBADF);
        }
        match accept_socket(lfd) {
            Ok(new_fd) => {
                if let Err(e) = self.after_accept(new_fd) {
                    return Some(-e);
                }
                let sidx = sock.0 as usize;
                self.socks[sidx].reset();
                self.socks[sidx].fd = new_fd;
                self.socks[sidx].kind = SockKind::Open;
                Some(0)
            }
            Err(e) if e == OS_EAGAIN || e == OS_EWOULDBLOCK => {
                self.socks[lidx].read_ready = false;
                self.poller.arm_read(lidx, &mut self.socks[lidx]);
                None
            }
            Err(e) => Some(to_linux_errno(e)),
        }
    }

    fn attempt_send(
        &mut self,
        sock: Sock,
        pool: &[u8],
        range: core::ops::Range<usize>,
    ) -> Option<i32> {
        let idx = sock.0 as usize;
        let fd = self.socks[idx].fd;
        if fd < 0 {
            return Some(-errno::linux::EPIPE);
        }
        let ptr: *const c_void = pool[range.clone()].as_ptr().cast();
        let len = range.len();
        loop {
            let n = unsafe { send(fd, ptr, len, MSG_SEND_FLAGS) };
            if n >= 0 {
                return Some(n as i32);
            }
            let e = os_errno();
            if e == OS_EINTR {
                continue;
            }
            if e == OS_EAGAIN || e == OS_EWOULDBLOCK {
                self.socks[idx].write_ready = false;
                self.poller.arm_write(idx, &mut self.socks[idx]);
                return None;
            }
            return Some(to_linux_errno(e));
        }
    }

    fn attempt_recv(
        &mut self,
        sock: Sock,
        pool: &mut [u8],
        range: core::ops::Range<usize>,
        no_wait: bool,
    ) -> Option<i32> {
        let idx = sock.0 as usize;
        let fd = self.socks[idx].fd;
        if fd < 0 {
            return Some(-errno::linux::EBADF);
        }
        let ptr: *mut c_void = pool[range.clone()].as_mut_ptr().cast();
        let len = range.len();
        loop {
            let n = unsafe { recv(fd, ptr, len, MSG_DONTWAIT) };
            if n >= 0 {
                return Some(n as i32);
            }
            let e = os_errno();
            if e == OS_EINTR {
                continue;
            }
            if e == OS_EAGAIN || e == OS_EWOULDBLOCK {
                self.socks[idx].read_ready = false;
                if no_wait {
                    return Some(-errno::linux::EAGAIN);
                }
                self.poller.arm_read(idx, &mut self.socks[idx]);
                return None;
            }
            return Some(to_linux_errno(e));
        }
    }

    fn attempt_read_file(
        &mut self,
        file_idx: usize,
        pool: &mut [u8],
        range: core::ops::Range<usize>,
    ) -> Option<i32> {
        let path = self.files[file_idx].as_ptr();
        let fd = loop {
            let fd = unsafe { open(path, O_RDONLY | O_CLOEXEC, 0) };
            if fd >= 0 {
                break fd;
            }
            let e = os_errno();
            if e == OS_EINTR {
                continue;
            }
            return Some(to_linux_errno(e));
        };
        let res = loop {
            let n = unsafe { pread(fd, pool[range.clone()].as_mut_ptr().cast(), range.len(), 0) };
            if n >= 0 {
                break n as i32;
            }
            let e = os_errno();
            if e == OS_EINTR {
                continue;
            }
            break to_linux_errno(e);
        };
        let _: i32 = unsafe { close(fd) };
        Some(res)
    }
}
