// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! [`Io`] on io_uring: one ring, one thread, registered buffers and files.

use crate::io::files::FileTable;
use crate::io::ring::Ring;
use crate::io::sys::*;
use crate::io::{
    BufId, Busy, Completion, Completions, FileId, FileSlot, Io, Limits, Op, Sock, Token, ECANCELED,
    ETIME,
};
use core::mem::ManuallyDrop;
use std::ffi::CString;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::os::fd::IntoRawFd;

mod backend;
mod completions;
#[cfg(feature = "fault-injection")]
mod fault;
#[cfg(feature = "fault-injection")]
pub use fault::Fault;
mod ops;
mod ops_file;
mod setup;
#[cfg(test)]
mod tests;

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
    /// Single-shot accept in flight.
    Accept,
    /// Armed multishot accept in flight.
    #[allow(dead_code)] // Wired in M4b.
    ArmedAccept,
    /// Armed multishot receive in flight.
    #[allow(dead_code)] // Wired in M4b.
    ArmedRecv,
    /// Zero-copy send in flight (waiting for send result and/or notif).
    SendZc,
    /// A timer. Its normal expiry arrives as `-ETIME`.
    Timer,
    /// Waiting on the wake-up eventfd. A read of its 8 bytes is a wake.
    Wake,
    /// Zero-copy send: result seen, waiting for the kernel to release the buffer.
    AwaitNotif,
    /// Zero-copy send: release seen first, waiting for the result.
    AwaitResult,
    /// Reading a legacy `FileId` file: the open, the read, then the close are in flight in turn.
    FileOpen,
    FileRead,
    FileClose,
    /// New registered-slot file operations.
    FileSlotOp,
    FileSlotOpen,
    FileSlotClose,
    /// Closing a socket slot.
    Close,
}

#[derive(Clone, Copy)]
struct Slot {
    token: u64,
    state: State,
    result: i32,
    sock: u32,
    is_stream: bool,
    file_slot: FileSlot,
    file_direct: bool,
    /// The socket slot this operation is in flight on, if any (`Accept` uses
    /// `listener`; `Close` and non-socket ops leave this `None`).
    op_sock: Option<Sock>,
    /// True if an [`Op::Close`] on `op_sock` has already submitted the close.
    sock_closed: bool,
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

pub(super) struct ReadyQueue {
    items: Box<[(u32, Completion)]>,
    head: usize,
    len: usize,
}

impl ReadyQueue {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            items: vec![(0, Completion::new(Token(0), 0)); capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn push(&mut self, slot_idx: u32, c: Completion) {
        let cap = self.items.len();
        let tail = (self.head + self.len) % cap;
        self.items[tail] = (slot_idx, c);
        self.len += 1;
    }

    pub(super) fn pop(&mut self) -> Option<(u32, Completion)> {
        if self.len == 0 {
            return None;
        }
        let item = self.items[self.head];
        self.head = (self.head + 1) % self.items.len();
        self.len -= 1;
        Some(item)
    }

    fn clear(&mut self, slots: &mut [Slot], free: &mut Vec<u32>) {
        while let Some((slot_i, _)) = self.pop() {
            slots[slot_i as usize].state = State::Free;
            free.push(slot_i);
        }
    }
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
    pool_base: usize,
    buf_range: Box<[u16]>,
    slots: Box<[Slot]>,
    sock_stream: Box<[bool]>,
    /// Completions the kernel still owes: one per SQE pushed, and one more
    /// after each that says more follow (a zero-copy send's notification).
    in_flight: u32,
    /// The wait bound of `quiesce`'s timer.
    quiet_ts: Box<timespec>,
    free: Vec<u32>,
    ready: ReadyQueue,
    files_table: FileTable,
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
/// Internal cancel submitted by `Op::Close` for sibling ops on the socket.
const INTERNAL_CANCEL: u64 = 7 << 32;
/// Internal shutdown submitted by `Op::Close` before closing a connected stream socket.
const INTERNAL_SHUTDOWN: u64 = 8 << 32;

/// The other threads' end of the wake-up eventfd.
struct EventFd(i32);

impl crate::io::Wake for EventFd {
    fn wake(&self) {
        let one: u64 = 1;
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
    /// Cancel every operation in flight and reap it, so the kernel touches
    /// no buffer or slot afterwards. Completions are discarded. True once
    /// nothing is in flight; false if the ring fails or something has not
    /// ended within five seconds (a zero-copy send whose data the peer never
    /// acknowledges can hold its buffer that long). Dropping calls this.
    pub fn quiesce(&mut self) -> bool {
        self.ready.clear(&mut self.slots, &mut self.free);
        if self.in_flight == 0 {
            return true;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Operations of several steps do not start their next step.
        for s in self.slots.iter_mut() {
            s.cancel_requested = true;
        }
        let mut timing = false;
        let mut out = Completions::new(8);
        while self.in_flight > 0 {
            if std::time::Instant::now() > deadline {
                return false;
            }
            for k in 0..self.slots.len() {
                let s = &self.slots[k];
                let opcode = match s.state {
                    State::Free
                    | State::Close
                    | State::FileClose
                    | State::FileSlotClose
                    | State::ConnectRelease
                    | State::AwaitNotif => continue,
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
                    QUIESCE | INTERNAL_CANCEL | INTERNAL_SHUTDOWN => {}
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
        Box::leak(core::mem::take(&mut self.buf_range));
        Box::leak(core::mem::take(&mut self.ready.items));
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
        self.bufs.as_ptr() as u64
            + self.pool_base as u64
            + self.limits.range(buf, off, len).start as u64
    }

    fn fixed(&self, sock: Sock) -> io_uring_sqe {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        io_uring_sqe {
            fd: sock.0 as i32,
            flags: IOSQE_FIXED_FILE,
            ..Default::default()
        }
    }

    fn fixed_file(&self, slot: FileSlot) -> io_uring_sqe {
        assert!(slot.0 < self.limits.files_max, "file slot out of range");
        io_uring_sqe {
            fd: (self.limits.sockets_max + slot.0) as i32,
            flags: IOSQE_FIXED_FILE,
            ..Default::default()
        }
    }

    fn push(&mut self, sqe: io_uring_sqe) {
        self.ring
            .push(sqe)
            .expect("submission queue sized for ops_max");
        self.in_flight += 1;
    }
}
