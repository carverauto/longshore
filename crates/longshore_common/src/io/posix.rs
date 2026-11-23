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
#[cfg(target_os = "macos")]
mod kqueue;
mod ops;
mod ops_armed;
mod sock;

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
    no_delay: bool,
    keep_alive: bool,
    #[cfg(feature = "fault-injection")]
    stale: [(Sock, bool); 64],
    #[cfg(feature = "fault-injection")]
    stale_len: usize,
}

impl Posix {
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
            no_delay: false,
            keep_alive: false,
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
                let _: i32 = unsafe { close(s.fd) };
                s.reset();
            }
        }
        for fd in self.file_fds.iter_mut() {
            if *fd >= 0 {
                let _: i32 = unsafe { close(*fd) };
                *fd = -1;
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
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

impl World for Posix {
    fn now_ns(&self) -> u64 {
        monotonic_ns()
    }

    fn unix_secs(&self) -> u64 {
        let mut ts = timespec::default();
        let _: i32 = unsafe { clock_gettime(CLOCK_REALTIME, &mut ts) };
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

    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        self.listen_unix_sock(sock, path)
    }

    fn no_delay(&mut self, on: bool) {
        self.no_delay = on;
    }

    fn keep_alive(&mut self, on: bool) {
        self.keep_alive = on;
    }

    fn give_back(&mut self, id: BufId) {
        self.give_back_ring(id);
    }

    fn waker(&mut self, woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        self.poller.waker(woken)
    }

    fn is_ready(&self, op: &Op) -> bool {
        match *op {
            Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. } | Op::Send { sock, .. } => {
                let s = &self.socks[sock.0 as usize];
                s.fd < 0 || s.write_ready
            }
            Op::Accept { listener, .. } | Op::ArmedAccept { listener } => {
                let s = &self.socks[listener.0 as usize];
                s.fd < 0 || s.read_ready
            }
            Op::Recv { sock, .. } | Op::RecvNow { sock, .. } | Op::ArmedRecv { sock } => {
                let s = &self.socks[sock.0 as usize];
                s.fd < 0 || s.read_ready
            }
            Op::ConnectUdp { .. }
            | Op::Close { .. }
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::AwaitWake
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => true,
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

    fn attempt_completion(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        _tag: u64,
    ) -> Option<Attempt> {
        match *op {
            Op::ArmedAccept { listener } => self.attempt_accept_armed(listener),
            Op::ArmedRecv { sock } => self.attempt_recv_armed(sock, pool, limits),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => self.attempt_op(op, pool, limits).map(Attempt::single),
        }
    }

    fn attempt_connect_unix(&mut self, sock: Sock, path_with_nul: &[u8]) -> Option<i32> {
        self.connect_unix(sock, path_with_nul)
    }

    fn claimed(&self, op: &Op) -> bool {
        matches!(
            op,
            Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. }
                if self.socks[sock.0 as usize].kind == SockKind::Connecting
        )
    }

    fn release(&mut self, op: &Op) {
        if let Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. } = op {
            let s = &mut self.socks[sock.0 as usize];
            if s.kind == SockKind::Connecting && s.fd >= 0 {
                self.poller.before_close(s);
                let _: i32 = unsafe { close(s.fd) };
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
            Op::Accept { listener, .. } | Op::ArmedAccept { listener } => {
                let s = &mut self.socks[listener.0 as usize];
                if s.fd >= 0 && !s.read_ready {
                    s.want_read = true;
                }
            }
            Op::Recv { sock, .. } | Op::ArmedRecv { sock } => {
                let s = &mut self.socks[sock.0 as usize];
                if s.fd >= 0 && !s.read_ready {
                    s.want_read = true;
                }
            }
            Op::Connect { sock, .. } | Op::ConnectUnix { sock, .. } | Op::Send { sock, .. } => {
                let s = &mut self.socks[sock.0 as usize];
                if s.fd >= 0 && !s.write_ready {
                    s.want_write = true;
                }
            }
            Op::ConnectUdp { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => {}
        }
    }

    fn wait(&mut self, deadline: Option<u64>, _only_timers: bool, _idle: u32) -> io::Result<()> {
        self.poller.wait(&mut self.socks, deadline, monotonic_ns)
    }
}
