// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! [`Io`] on io_uring: one ring, one thread, registered buffers and files.

use crate::io::ring::Ring;
use crate::io::sys::*;
use crate::io::{
    BufId, Busy, Completion, Completions, FileId, Io, Limits, Op, Sock, Token, ECANCELED, ETIME,
};
use std::ffi::CString;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::os::fd::IntoRawFd;

mod backend;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Free,
    InFlight,
    /// Socket created; the connect SQE is submitted when that completes.
    Connecting,
    /// The connect itself is in flight.
    ConnectStep,
    /// The connect failed; the socket it created is being closed so that
    /// its slot is free again before the failure is reported.
    ConnectRelease,
    /// A timer. Its normal expiry arrives as `-ETIME`.
    Timer,
    /// Waiting on the wake-up eventfd. A read of its 8 bytes is a wake.
    Wake,
    /// Zero-copy send: result seen, waiting for the kernel to release the buffer.
    AwaitNotif,
    /// Zero-copy send: release seen first, waiting for the result.
    AwaitResult,
    /// Reading a file: the open, the read, then the close are in flight in turn.
    FileOpen,
    FileRead,
    FileClose,
}

#[derive(Clone, Copy)]
struct Slot {
    token: u64,
    state: State,
    result: i32,
    sock: u32,
    /// For a file read: the descriptor, and where the bytes go.
    fd: i32,
    file_addr: u64,
    file_len: u32,
    /// Phase bits of the SQE currently in flight for this slot, so a cancel
    /// can name it.
    phase: u64,
    /// A cancel has been submitted for this operation. See `Op::Cancel`.
    cancel_requested: bool,
    // Kernel reads these while the operation is in flight, so they live here.
    addr: sockaddr_any,
    addr_len: u32,
    /// The process's own descriptor for a socket whose segment size is to
    /// be read once it connects, or -1. See `measure_segment_sizes`.
    probe_fd: i32,
    ts: timespec,
    /// Where the kernel puts the eventfd's counter.
    wake_count: u64,
}

pub struct UringIo {
    ring: Ring,
    limits: Limits,
    bufs: Box<[u8]>,
    slots: Box<[Slot]>,
    free: Vec<u32>,
    /// Paths registered for `Op::ReadFile`, NUL-terminated for the kernel.
    files: Vec<CString>,
    /// The eventfd behind `Io::waker`, or -1 before it is asked for.
    wake_fd: i32,
    measure_segments: bool,
}

/// Marks the CQE of the socket-creation half of a connect.
const PHASE_SOCKET: u64 = 1 << 32;
const PHASE_FILE_OPEN: u64 = 2 << 32;
const PHASE_FILE_READ: u64 = 3 << 32;
const PHASE_FILE_CLOSE: u64 = 4 << 32;
const PHASE_SOCK_RELEASE: u64 = 5 << 32;

mod completions;

/// The other threads' end of the wake-up eventfd.
struct EventFd(i32);

impl crate::io::Wake for EventFd {
    fn wake(&self) {
        let one: u64 = 1;
        // The counter cannot overflow in practice, and a failed write only
        // means the reader is already due to wake.
        unsafe { write(self.0, (&one as *const u64).cast(), 8) };
    }
}

impl Drop for EventFd {
    fn drop(&mut self) {
        unsafe { close(self.0) };
    }
}

impl Drop for UringIo {
    fn drop(&mut self) {
        if self.wake_fd >= 0 {
            unsafe { close(self.wake_fd) };
        }
    }
}

impl UringIo {
    /// Allocates and registers everything. Call on the thread that will use it.
    pub fn new(limits: Limits) -> io::Result<UringIo> {
        let entries = limits.ops_max.next_power_of_two().max(8);
        let mut ring = Ring::new(
            entries,
            IORING_SETUP_SINGLE_ISSUER | IORING_SETUP_DEFER_TASKRUN,
        )?;
        let mut bufs = vec![0u8; limits.pool_bytes()].into_boxed_slice();
        // One registered buffer per pool buffer, so a buffer's id is its registered index.
        let iov: Vec<iovec> = (0..limits.buffer_count())
            .map(|id| {
                let (at, len) = limits.span(BufId(id));
                iovec {
                    iov_base: bufs[at..at + len].as_mut_ptr().cast(),
                    iov_len: len,
                }
            })
            .collect();
        ring.register_buffers(&iov)?;
        ring.register_files(&vec![-1i32; limits.sockets_max as usize])?;
        let slot = Slot {
            cancel_requested: false,
            token: 0,
            state: State::Free,
            result: 0,
            sock: 0,
            fd: -1,
            file_addr: 0,
            file_len: 0,
            phase: 0,
            addr: sockaddr_any::default(),
            addr_len: 0,
            probe_fd: -1,
            ts: timespec::default(),
            wake_count: 0,
        };
        Ok(UringIo {
            ring,
            limits,
            bufs,
            slots: vec![slot; limits.ops_max as usize].into_boxed_slice(),
            free: (0..limits.ops_max).rev().collect(),
            files: Vec::new(),
            wake_fd: -1,
            measure_segments: false,
        })
    }

    /// `io_uring_enter` calls made so far.
    pub fn enters(&self) -> u64 {
        self.ring.enters
    }

    fn buf_ptr(&self, buf: BufId, off: u32, len: u32) -> u64 {
        self.bufs.as_ptr() as u64 + self.limits.range(buf, off, len).start as u64
    }

    fn fixed(&self, sock: Sock) -> io_uring_sqe {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        io_uring_sqe {
            fd: sock.0 as i32,
            flags: IOSQE_FIXED_FILE,
            ..Default::default()
        }
    }

    fn push(&mut self, sqe: io_uring_sqe) {
        // The SQ has at least `ops_max` entries and each slot has at most one
        // SQE outstanding, so this cannot fail.
        self.ring
            .push(sqe)
            .expect("submission queue sized for ops_max");
    }
}
