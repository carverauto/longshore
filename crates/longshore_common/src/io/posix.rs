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
#[cfg(target_os = "macos")]
mod kqueue;
mod ops;
mod sock;

#[cfg(target_os = "linux")]
use abi_linux::*;
#[cfg(target_os = "macos")]
use abi_macos::*;
#[cfg(target_os = "linux")]
use epoll::Poller;
#[cfg(target_os = "macos")]
use kqueue::Poller;

use crate::io::engine::{Engine, World};
use crate::io::{FileId, Limits, Op, Sock, Wake};
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
        }
    }

    fn reset(&mut self) {
        *self = Self::free();
    }
}

/// Non-blocking POSIX I/O world backed by `kqueue` (macOS) or `epoll` (Linux).
pub struct Posix {
    socks: Box<[SockSlot]>,
    files: Vec<CString>,
    poller: Poller,
    cursor: usize,
    #[cfg(feature = "fault-injection")]
    stale: [(Sock, bool); 64],
    #[cfg(feature = "fault-injection")]
    stale_len: usize,
}

impl Posix {
    pub fn new(limits: Limits) -> io::Result<Self> {
        Ok(Self {
            socks: (0..limits.sockets_max).map(|_| SockSlot::free()).collect(),
            files: Vec::new(),
            poller: Poller::new()?,
            cursor: 0,
            #[cfg(feature = "fault-injection")]
            stale: [(Sock(0), false); 64],
            #[cfg(feature = "fault-injection")]
            stale_len: 0,
        })
    }
}

impl Drop for Posix {
    fn drop(&mut self) {
        for s in self.socks.iter_mut() {
            if s.fd >= 0 {
                self.poller.before_close(s);
                unsafe { close(s.fd) };
                s.reset();
            }
        }
    }
}

/// Portable POSIX backend built on [`Engine`] and [`Posix`].
pub type PosixIo = Engine<Posix>;

impl PosixIo {
    /// Allocate the buffer pool, socket table, and kernel poller (`kqueue`/`epoll`).
    pub fn new(limits: Limits) -> io::Result<PosixIo> {
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
}

pub(super) fn monotonic_ns() -> u64 {
    let mut ts = timespec::default();
    unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

impl World for Posix {
    fn now_ns(&self) -> u64 {
        monotonic_ns()
    }

    fn unix_secs(&self) -> u64 {
        let mut ts = timespec::default();
        unsafe { clock_gettime(CLOCK_REALTIME, &mut ts) };
        ts.tv_sec as u64
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.files
            .push(CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?);
        Ok(FileId(self.files.len() as u32 - 1))
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.listen_sock(sock, addr)
    }

    fn waker(&mut self, woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        self.poller.waker(woken)
    }

    fn is_ready(&self, op: &Op) -> bool {
        match *op {
            Op::Connect { sock, .. } | Op::Send { sock, .. } => {
                let s = &self.socks[sock.0 as usize];
                s.fd < 0 || s.write_ready
            }
            Op::Accept { listener, .. } => {
                let s = &self.socks[listener.0 as usize];
                s.fd < 0 || s.read_ready
            }
            Op::Recv { sock, .. } | Op::RecvNow { sock, .. } => {
                let s = &self.socks[sock.0 as usize];
                s.fd < 0 || s.read_ready
            }
            Op::ConnectUdp { .. }
            | Op::Close { .. }
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::AwaitWake
            | Op::Cancel { .. } => true,
        }
    }

    fn advance(&mut self) {
        self.poller.poll(&mut self.socks);
        #[cfg(feature = "fault-injection")]
        {
            for i in 0..self.stale_len {
                let (sock, write) = self.stale[i];
                if let Some(s) = self.socks.get_mut(sock.0 as usize) {
                    if s.fd >= 0 {
                        if write {
                            s.write_ready = true;
                        } else {
                            s.read_ready = true;
                        }
                    }
                }
            }
            self.stale_len = 0;
        }
    }

    fn first(&mut self, n: usize) -> usize {
        let start = self.cursor % n;
        self.cursor = self.cursor.wrapping_add(1);
        start
    }

    fn hold(&mut self, _op: &Op) -> bool {
        false
    }

    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.attempt_op(op, pool, limits)
    }

    fn claimed(&self, op: &Op) -> bool {
        matches!(
            op,
            Op::Connect { sock, .. } if self.socks[sock.0 as usize].kind == SockKind::Connecting
        )
    }

    fn release(&mut self, op: &Op) {
        if let Op::Connect { sock, .. } = op {
            let s = &mut self.socks[sock.0 as usize];
            if s.kind == SockKind::Connecting && s.fd >= 0 {
                self.poller.before_close(s);
                unsafe { close(s.fd) };
                s.reset();
            }
        }
    }

    fn closed(&mut self, _sock: Sock) {}

    fn clear_interest(&mut self) {
        for s in self.socks.iter_mut() {
            s.want_read = false;
            s.want_write = false;
        }
    }

    fn add_interest(&mut self, op: &Op) {
        match *op {
            Op::Accept { listener, .. } => {
                let s = &mut self.socks[listener.0 as usize];
                if s.fd >= 0 && !s.read_ready {
                    s.want_read = true;
                }
            }
            Op::Recv { sock, .. } => {
                let s = &mut self.socks[sock.0 as usize];
                if s.fd >= 0 && !s.read_ready {
                    s.want_read = true;
                }
            }
            Op::Connect { sock, .. } | Op::Send { sock, .. } => {
                let s = &mut self.socks[sock.0 as usize];
                if s.fd >= 0 && !s.write_ready {
                    s.want_write = true;
                }
            }
            _ => {}
        }
    }

    fn wait(&mut self, deadline: Option<u64>, _only_timers: bool, _idle: u32) -> io::Result<()> {
        self.poller.wait(&mut self.socks, deadline, monotonic_ns)
    }
}
