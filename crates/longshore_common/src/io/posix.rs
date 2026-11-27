// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Portable POSIX [`World`] (`kqueue` on macOS, `epoll` on Linux) and [`PosixIo`].

#[cfg(target_os = "linux")]
mod abi_linux;
#[cfg(target_os = "macos")]
mod abi_macos;
#[cfg(target_os = "linux")]
mod epoll;
pub mod errno;
mod files;
mod gather;
#[cfg(target_os = "macos")]
mod kqueue;
mod ops;
mod ops_armed;
mod sock;
mod world;

#[cfg(target_os = "linux")]
use abi_linux::*;
#[cfg(target_os = "macos")]
use abi_macos::*;
#[cfg(target_os = "linux")]
use epoll::Poller;
#[cfg(target_os = "macos")]
use kqueue::Poller;

use crate::io::engine::{Attempt, Engine, World};
use crate::io::{BufId, FileId, Limits, Op, Sock, Wake};
use std::ffi::CString;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SockKind {
    Free,
    Listening,
    Connecting,
    Open,
    Udp,
}

#[derive(Clone, Copy)]
struct SockSlot {
    fd: i32,
    kind: SockKind,
    read_ready: bool,
    write_ready: bool,
    armed_read: bool,
    armed_write: bool,
    want_read: bool,
    want_write: bool,
    /// Every send ends a buffer (`Io::end_sends`).
    eor: bool,
}

impl SockSlot {
    fn free() -> Self {
        Self {
            fd: -1,
            kind: SockKind::Free,
            read_ready: false,
            write_ready: false,
            armed_read: false,
            armed_write: false,
            want_read: false,
            want_write: false,
            eor: false,
        }
    }

    fn reset(&mut self) {
        *self = Self::free();
    }
}

/// Non-blocking POSIX I/O world backed by `kqueue` (macOS) or `epoll` (Linux).
pub struct Posix {
    socks: Box<[SockSlot]>,
    file_fds: Box<[i32]>,
    files: Vec<CString>,
    poller: Poller,
    cursor: usize,
    ring_base: u32,
    ring_free: Box<[u16]>,
    ring_free_len: usize,
    ring_in_free: Box<[bool]>,
    accept_start: u32,
    accept_len: u32,
    keep_timing: Option<crate::io::KeepAlive>,
    no_delay: bool,
    keep_alive: bool,
    /// The vector of a gathered write.
    gather: Box<[gather::Iov]>,
    #[cfg(feature = "fault-injection")]
    stale: [(Sock, bool); 64],
    #[cfg(feature = "fault-injection")]
    stale_len: usize,
}

impl Posix {
    /// A world with the slots `limits` asks for, all free. Allocates them, and
    /// opens the poller.
    pub fn new(limits: Limits) -> io::Result<Self> {
        let ring_count = limits.ring_buffers as usize;
        Ok(Self {
            socks: (0..limits.sockets_max).map(|_| SockSlot::free()).collect(),
            file_fds: vec![-1; limits.files_max as usize].into_boxed_slice(),
            files: Vec::new(),
            poller: Poller::new()?,
            cursor: 0,
            ring_base: limits.ring_base(),
            ring_free: (0..limits.ring_buffers as u16).rev().collect(),
            ring_free_len: ring_count,
            ring_in_free: vec![true; ring_count].into_boxed_slice(),
            accept_start: limits.accept_range_start,
            accept_len: limits.accept_range_len,
            keep_timing: None,
            no_delay: false,
            keep_alive: false,
            gather: gather::vector(&limits),
            #[cfg(feature = "fault-injection")]
            stale: [(Sock(0), false); 64],
            #[cfg(feature = "fault-injection")]
            stale_len: 0,
        })
    }
}

/// Portable POSIX backend built on [`Engine`] and [`Posix`].
pub type PosixIo = Engine<Posix>;

impl PosixIo {
    /// Allocate the buffer pool, socket table, and kernel poller (`kqueue`/`epoll`).
    pub fn new(limits: Limits) -> io::Result<PosixIo> {
        if limits.area_pages() > 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the portable backend has no zero-copy receive: an area of pages \
                 (Limits::area_pages) needs the io_uring backend on Linux",
            ));
        }
        Ok(Engine::with_world(limits, Posix::new(limits)?))
    }

    /// Inject a spurious readiness notification for `sock` (`write == true` for
    /// write readiness, `false` for read readiness) on the next poll.
    #[cfg(feature = "fault-injection")]
    pub fn inject_stale(&mut self, sock: Sock, write: bool) {
        let w = self.world_mut();
        if w.stale_len < w.stale.len() {
            w.stale[w.stale_len] = (sock, write);
            w.stale_len += 1;
        }
    }

    /// Read the keepalive `(idle, interval, count)` from the underlying
    /// descriptor of `sock`.
    pub fn keep_timing(&self, sock: Sock) -> io::Result<(u32, u32, u32)> {
        let s = self
            .world()
            .socks
            .get(sock.0 as usize)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if s.fd < 0 {
            return Err(io::Error::from(io::ErrorKind::NotConnected));
        }
        sock::read_keep_timing(s.fd)
    }

    /// Read `(TCP_NODELAY, SO_KEEPALIVE)` from the underlying descriptor of `sock`.
    pub fn tcp_options(&self, sock: Sock) -> io::Result<(bool, bool)> {
        let s = self
            .world()
            .socks
            .get(sock.0 as usize)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if s.fd < 0 {
            return Err(io::Error::from(io::ErrorKind::NotConnected));
        }
        sock::read_tcp_options(s.fd)
    }
}

pub(super) fn monotonic_ns() -> u64 {
    let mut ts = timespec::default();
    let _: i32 = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    crate::io::clock_ns(ts.tv_sec, ts.tv_nsec)
}
