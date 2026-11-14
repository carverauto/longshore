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
use core::mem::ManuallyDrop;
use std::ffi::CString;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::os::fd::IntoRawFd;

mod backend;
#[cfg(feature = "fault-injection")]
mod fault;
#[cfg(feature = "fault-injection")]
pub use fault::Fault;

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

/// The kernel holds pointers into memory this owns while operations are in
/// flight (the fields `UringIo::leak` lists), and closing the ring does not
/// wait for them to end. The rule: that memory is freed only when
/// `in_flight` is zero, and otherwise leaked, which is always safe.
/// Dropping calls [`UringIo::quiesce`], closes the ring and then the
/// eventfd, and frees that memory if it returned true or leaks it if it
/// returned false. A field the kernel can reach belongs in `leak`.
pub struct UringIo {
    ring: ManuallyDrop<Ring>,
    limits: Limits,
    bufs: Box<[u8]>,
    slots: Box<[Slot]>,
    /// Completions the kernel still owes: one per SQE pushed, and one more
    /// after each that says more follow (a zero-copy send's notification).
    in_flight: u32,
    /// The wait bound of `quiesce`'s timer.
    quiet_ts: Box<timespec>,
    free: Vec<u32>,
    /// Paths registered for `Op::ReadFile`, NUL-terminated for the kernel.
    files: Vec<CString>,
    /// The eventfd behind `Io::waker`, or -1 before it is asked for.
    wake_fd: i32,
    measure_segments: bool,
    /// The next `tick` reports a failure. See [`Fault::TickFails`].
    #[cfg(feature = "fault-injection")]
    fail_tick: bool,
}

/// Marks the CQE of the socket-creation half of a connect.
const PHASE_SOCKET: u64 = 1 << 32;
const PHASE_FILE_OPEN: u64 = 2 << 32;
const PHASE_FILE_READ: u64 = 3 << 32;
const PHASE_FILE_CLOSE: u64 = 4 << 32;
const PHASE_SOCK_RELEASE: u64 = 5 << 32;
/// The cancel and timer `quiesce` submits, which belong to no slot.
const QUIESCE: u64 = 6 << 32;

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
        let quiet = self.quiesce();
        unsafe { ManuallyDrop::drop(&mut self.ring) };
        if self.wake_fd >= 0 {
            unsafe { close(self.wake_fd) };
        }
        if !quiet {
            self.leak();
        }
    }
}

impl UringIo {
    /// Allocates and registers everything. Call on the thread that will use it.
    pub fn new(limits: Limits) -> io::Result<UringIo> {
        // Room for one SQE per operation, and for `quiesce`'s cancel and timer.
        let entries = (limits.ops_max + 2).next_power_of_two().max(8);
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
            ring: ManuallyDrop::new(ring),
            limits,
            bufs,
            slots: vec![slot; limits.ops_max as usize].into_boxed_slice(),
            in_flight: 0,
            quiet_ts: Box::new(timespec {
                sec: 0,
                nsec: 100_000_000,
            }),
            free: (0..limits.ops_max).rev().collect(),
            files: Vec::new(),
            wake_fd: -1,
            measure_segments: false,
            #[cfg(feature = "fault-injection")]
            fail_tick: false,
        })
    }

    /// Cancel every operation in flight and reap it, so the kernel touches
    /// no buffer or slot afterwards. Completions are discarded. True once
    /// nothing is in flight; false if the ring fails or something has not
    /// ended within five seconds (a zero-copy send whose data the peer never
    /// acknowledges can hold its buffer that long). Dropping calls this.
    pub fn quiesce(&mut self) -> bool {
        if self.in_flight == 0 {
            return true;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Operations of several steps do not start their next step.
        for s in self.slots.iter_mut() {
            s.cancel_requested = true;
        }
        // Each round cancels every operation still in flight by its own
        // identity, then waits for one completion or 100 ms and reaps what
        // came, so operations of several steps advance as they normally do.
        let mut timing = false;
        let mut out = Completions::new(8);
        while self.in_flight > 0 {
            if std::time::Instant::now() > deadline {
                return false;
            }
            for k in 0..self.slots.len() {
                let s = &self.slots[k];
                let opcode = match s.state {
                    // A close is never cancelled, and a zero-copy send whose
                    // result is in only waits for its notification.
                    State::Free | State::FileClose | State::ConnectRelease | State::AwaitNotif => {
                        continue
                    }
                    State::Timer => IORING_OP_TIMEOUT_REMOVE,
                    _ => IORING_OP_ASYNC_CANCEL,
                };
                let cancel = io_uring_sqe {
                    opcode,
                    fd: -1,
                    addr: k as u64 | s.phase,
                    user_data: QUIESCE,
                    ..Default::default()
                };
                if !self.push_flushing(cancel) {
                    return false;
                }
            }
            if !timing {
                let timer = io_uring_sqe {
                    opcode: IORING_OP_TIMEOUT,
                    addr: &*self.quiet_ts as *const timespec as u64,
                    len: 1,
                    user_data: QUIESCE | 1,
                    ..Default::default()
                };
                if !self.push_flushing(timer) {
                    return false;
                }
                timing = true;
            }
            if self.ring.enter(1).is_err() {
                return false;
            }
            while let Some(c) = self.reap() {
                match c.user_data {
                    QUIESCE => {}
                    t if t == QUIESCE | 1 => timing = false,
                    _ => {
                        self.on_cqe(c, &mut out);
                        while out.pop().is_some() {}
                    }
                }
            }
        }
        true
    }

    /// Everything the kernel may still read or write, given up for good
    /// because it may still be in use.
    fn leak(&mut self) {
        Box::leak(core::mem::take(&mut self.slots));
        Box::leak(core::mem::take(&mut self.bufs));
        Box::leak(core::mem::take(&mut self.quiet_ts));
        core::mem::forget(core::mem::take(&mut self.files));
    }

    /// Push for `quiesce`, which must not panic: if the queue is full, hand
    /// it to the kernel and try once more. False if that fails.
    fn push_flushing(&mut self, sqe: io_uring_sqe) -> bool {
        let pushed = self.ring.push(sqe).is_ok()
            || (self.ring.enter(0).is_ok() && self.ring.push(sqe).is_ok());
        self.in_flight += pushed as u32;
        pushed
    }

    /// Take one completion, keeping `in_flight` exact.
    fn reap(&mut self) -> Option<io_uring_cqe> {
        let c = self.ring.pop()?;
        self.in_flight -= 1;
        if c.flags & IORING_CQE_F_MORE != 0 {
            self.in_flight += 1;
        }
        Some(c)
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
        self.in_flight += 1;
    }
}
