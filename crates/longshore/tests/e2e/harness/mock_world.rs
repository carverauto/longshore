// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Configurable mock `World` for testing `longshore::io::engine::Engine<W>`.

use longshore::io::engine::World;
use longshore::io::{FileId, Limits, Op, Sock};
use std::cell::{Ref, RefCell};
use std::io;
use std::net::SocketAddr;

/// Simplified tag identifying an `Op` variant in hook call logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Connect,
    ConnectUdp,
    ConnectUnix,
    Accept,
    ArmedAccept,
    Send,
    Recv,
    ArmedRecv,
    RecvNow,
    Close,
    AwaitWake,
    ReadFile,
    Timeout,
    Cancel,
    FileOpen,
    FileClose,
    FileRead,
    FileWrite,
    FileSync,
    FileReserve,
    FilePunch,
    FileStatus,
    FileRename,
    FileUnlink,
    FileMkdir,
}

impl OpKind {
    /// Classifies an [`Op`] into its variant tag.
    pub fn of(op: &Op) -> Self {
        match op {
            Op::Connect { .. } => Self::Connect,
            Op::ConnectUdp { .. } => Self::ConnectUdp,
            Op::ConnectUnix { .. } => Self::ConnectUnix,
            Op::Accept { .. } => Self::Accept,
            Op::ArmedAccept { .. } => Self::ArmedAccept,
            Op::Send { .. } => Self::Send,
            Op::Recv { .. } => Self::Recv,
            Op::ArmedRecv { .. } => Self::ArmedRecv,
            Op::RecvNow { .. } => Self::RecvNow,
            Op::Close { .. } => Self::Close,
            Op::AwaitWake => Self::AwaitWake,
            Op::ReadFile { .. } => Self::ReadFile,
            Op::Timeout { .. } => Self::Timeout,
            Op::Cancel { .. } => Self::Cancel,
            Op::FileOpen { .. } => Self::FileOpen,
            Op::FileClose { .. } => Self::FileClose,
            Op::FileRead { .. } => Self::FileRead,
            Op::FileWrite { .. } => Self::FileWrite,
            Op::FileSync { .. } => Self::FileSync,
            Op::FileReserve { .. } => Self::FileReserve,
            Op::FilePunch { .. } => Self::FilePunch,
            Op::FileStatus { .. } => Self::FileStatus,
            Op::FileRename { .. } => Self::FileRename,
            Op::FileUnlink { .. } => Self::FileUnlink,
            Op::FileMkdir { .. } => Self::FileMkdir,
        }
    }
}

/// Exact hook invocation recorded by [`TestWorld`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookCall {
    Advance,
    First(usize),
    Hold(OpKind),
    Attempt(OpKind),
    Claimed(OpKind),
    Release(OpKind),
    Closed(Sock),
    Wait {
        deadline: Option<u64>,
        only_timers: bool,
        idle: u32,
    },
}

type AttemptHandler = Box<dyn FnMut(&Op, &mut [u8], &Limits) -> Option<i32>>;

/// Configurable mock [`World`] implementation recording exact hook call order.
pub struct TestWorld {
    calls: RefCell<Vec<HookCall>>,
    now: u64,
    step_ns: u64,
    unix_epoch: u64,
    first_index: usize,
    hold_next: bool,
    ready_socks: Option<Vec<Sock>>,
    claimed_socks: Vec<Sock>,
    claim_connects: bool,
    stall_on_wait: bool,
    wait_limit: Option<usize>,
    wait_count: usize,
    attempt_handler: Option<AttemptHandler>,
}

impl Default for TestWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl TestWorld {
    /// Creates a new `TestWorld` with deterministic 100 us steps and no stalls.
    pub fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            now: 0,
            step_ns: 100_000,
            unix_epoch: 1_700_000_000,
            first_index: 0,
            hold_next: false,
            ready_socks: None,
            claimed_socks: Vec::new(),
            claim_connects: true,
            stall_on_wait: false,
            wait_limit: None,
            wait_count: 0,
            attempt_handler: None,
        }
    }

    /// Starts the clock at `now` and advances it by `step_ns` per reading,
    /// with the Unix time `unix_epoch` at the start.
    pub fn with_clock(mut self, now: u64, step_ns: u64, unix_epoch: u64) -> Self {
        (self.now, self.step_ns, self.unix_epoch) = (now, step_ns, unix_epoch);
        self
    }

    /// The index the world reports first.
    pub fn with_first_index(mut self, first_index: usize) -> Self {
        self.first_index = first_index;
        self
    }

    /// Claims operations on the specified socket slots.
    pub fn with_claimed_socks(mut self, socks: Vec<Sock>) -> Self {
        self.claimed_socks = socks;
        self
    }

    /// Restricts `is_ready` reporting to the specified socket slots.
    pub fn with_ready_socks(mut self, socks: Vec<Sock>) -> Self {
        self.ready_socks = Some(socks);
        self
    }

    /// Configures `wait` to simulate a stall by returning `ErrorKind::TimedOut`.
    pub fn with_stall(mut self, stall: bool) -> Self {
        self.stall_on_wait = stall;
        self
    }

    /// Installs a custom closure invoked on every `World::attempt`.
    pub fn with_attempt_handler<F>(mut self, f: F) -> Self
    where
        F: FnMut(&Op, &mut [u8], &Limits) -> Option<i32> + 'static,
    {
        self.attempt_handler = Some(Box::new(f));
        self
    }

    /// Clears the recorded hook call log.
    pub fn clear_calls(&self) {
        self.calls.borrow_mut().clear();
    }

    /// Borrows the recorded hook calls in invocation order.
    pub fn calls(&self) -> Ref<'_, [HookCall]> {
        Ref::map(self.calls.borrow(), |v| v.as_slice())
    }

    fn op_sock(op: &Op) -> Option<Sock> {
        match *op {
            Op::Connect { sock, .. }
            | Op::ConnectUdp { sock, .. }
            | Op::ConnectUnix { sock, .. }
            | Op::Accept { listener: sock, .. }
            | Op::ArmedAccept { listener: sock }
            | Op::Send { sock, .. }
            | Op::Recv { sock, .. }
            | Op::ArmedRecv { sock }
            | Op::RecvNow { sock, .. }
            | Op::Close { sock } => Some(sock),
            Op::AwaitWake
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
            | Op::FileMkdir { .. } => None,
        }
    }
}

impl World for TestWorld {
    fn now_ns(&self) -> u64 {
        self.now
    }

    fn unix_secs(&self) -> u64 {
        self.unix_epoch + self.now / 1_000_000_000
    }

    fn register_file(&mut self, _path: &str) -> io::Result<FileId> {
        Ok(FileId::new(0))
    }

    fn listen(&mut self, _sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        Ok(addr)
    }

    fn listen_unix(&mut self, _sock: Sock, _path: &str) -> io::Result<()> {
        Ok(())
    }

    fn is_ready(&self, op: &Op) -> bool {
        let Some(ref ready) = self.ready_socks else {
            return true;
        };
        match Self::op_sock(op) {
            Some(s) => ready.contains(&s),
            None => true,
        }
    }

    fn advance(&mut self) {
        self.calls.borrow_mut().push(HookCall::Advance);
        self.now = self.now.saturating_add(self.step_ns);
    }

    fn first(&mut self, n: usize) -> usize {
        self.calls.borrow_mut().push(HookCall::First(n));
        if n == 0 {
            0
        } else {
            self.first_index % n
        }
    }

    fn hold(&mut self, op: &Op) -> bool {
        self.calls.borrow_mut().push(HookCall::Hold(OpKind::of(op)));
        let hold = self.hold_next;
        self.hold_next = false;
        hold
    }

    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.calls
            .borrow_mut()
            .push(HookCall::Attempt(OpKind::of(op)));
        if let Some(ref mut handler) = self.attempt_handler {
            return handler(op, pool, limits);
        }
        match op {
            Op::Close { .. } => Some(0),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
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
            | Op::FileMkdir { .. } => None,
        }
    }

    fn claimed(&self, op: &Op) -> bool {
        self.calls
            .borrow_mut()
            .push(HookCall::Claimed(OpKind::of(op)));
        match op {
            Op::Connect { sock, .. } if self.claim_connects => {
                self.claimed_socks.is_empty() || self.claimed_socks.contains(sock)
            }
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
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
            | Op::FileMkdir { .. } => {
                Self::op_sock(op).is_some_and(|s| self.claimed_socks.contains(&s))
            }
        }
    }

    fn release(&mut self, op: &Op) {
        self.calls
            .borrow_mut()
            .push(HookCall::Release(OpKind::of(op)));
    }

    fn closed(&mut self, sock: Sock) {
        self.calls.borrow_mut().push(HookCall::Closed(sock));
    }

    fn wait(&mut self, deadline: Option<u64>, only_timers: bool, idle: u32) -> io::Result<()> {
        self.calls.borrow_mut().push(HookCall::Wait {
            deadline,
            only_timers,
            idle,
        });
        if self.stall_on_wait && !only_timers {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "simulated TestWorld deadlock",
            ));
        }
        if let Some(limit) = self.wait_limit {
            self.wait_count += 1;
            if self.wait_count > limit {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "simulated wait_limit exceeded",
                ));
            }
        }
        if let Some(dl) = deadline {
            if dl > self.now {
                self.now = dl;
            }
        } else {
            self.now = self.now.saturating_add(self.step_ns);
        }
        Ok(())
    }
}
