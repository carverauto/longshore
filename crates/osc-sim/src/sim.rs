// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use crate::prng::Prng;
use osc_io::{
    BufId, Busy, Completion, Completions, FileId, Io, Limits, Op, Sock, Token, EAGAIN, ECANCELED,
    ECONNREFUSED, ECONNRESET, ENOENT, EPIPE,
};
use std::io;
use std::net::SocketAddrV4;

mod backend;

/// Fault probabilities, in parts per million, evaluated per attempt.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    /// A connect is refused.
    pub connect_refused: u32,
    /// A send or receive resets the connection.
    pub reset: u32,
    /// A send accepts fewer bytes than asked.
    pub short_send: u32,
    /// A receive returns fewer bytes than are available.
    pub partial_recv: u32,
    /// One bit of a sent chunk is flipped in transit.
    pub bit_flip: u32,
    /// A ready operation is held back for this tick (delays and reorders).
    pub delay: u32,
    /// A UDP datagram is lost. Corrupted datagrams are also lost, as the UDP
    /// checksum would discard them.
    pub udp_drop: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum SockState {
    Free,
    Listening,
    /// Connect submitted, waiting for the listener to accept.
    Connecting,
    Open,
    /// UDP socket bound to `addr`; replies go to the sender of the last datagram received.
    UdpBound,
    /// UDP socket connected to `peer` (if anything is bound there).
    UdpConnected,
}

struct SimSock {
    state: SockState,
    addr: Option<SocketAddrV4>,
    peer: u32,
    /// Bytes in flight towards this socket.
    rx: Box<[u8]>,
    rx_head: usize,
    rx_len: usize,
    peer_closed: bool,
    reset: bool,
    /// For UDP: whether `peer` is valid.
    has_peer: bool,
}

#[derive(Clone, Copy)]
struct Pending {
    token: Token,
    op: Op,
    /// Absolute deadline for timeouts.
    deadline: u64,
    cancelled: bool,
    /// Identifies this operation among all ever submitted.
    id: u64,
    /// For a cancel: the operation it named when it was submitted, if any.
    /// The real kernel fixes the target at that moment too, so a cancel can
    /// neither hit an operation submitted later under the same token nor
    /// find one that has already finished.
    cancel_target: Option<u64>,
    /// A connect that has claimed its socket and is waiting to be accepted.
    attached: bool,
}

pub struct SimIo {
    limits: Limits,
    faults: Faults,
    rng: Prng,
    now: u64,
    bufs: Box<[u8]>,
    socks: Box<[SimSock]>,
    pending: Vec<Pending>,
    next_op_id: u64,
    trace: u64,
    /// Registered files and their current contents; `None` means absent.
    files: Vec<Option<Vec<u8>>>,
}

/// Virtual time added per tick.
const STEP_NS: u64 = 100_000;
/// Bytes a virtual socket holds before senders stall.
const SOCK_BUF: usize = 64 * 1024;
/// Ticks `tick(wait = true)` spins with nothing completing before reporting a deadlock.
const STALL_TICKS: u32 = 100_000;

mod engine;

impl SimIo {
    pub fn new(limits: Limits, seed: u64, faults: Faults) -> SimIo {
        let sock = || SimSock {
            state: SockState::Free,
            addr: None,
            peer: 0,
            rx: vec![0u8; SOCK_BUF].into_boxed_slice(),
            rx_head: 0,
            rx_len: 0,
            peer_closed: false,
            reset: false,
            has_peer: false,
        };
        SimIo {
            limits,
            faults,
            rng: Prng::new(seed),
            now: 0,
            bufs: vec![0u8; limits.pool_bytes()].into_boxed_slice(),
            socks: (0..limits.sockets_max).map(|_| sock()).collect(),
            pending: Vec::with_capacity(limits.ops_max as usize),
            next_op_id: 0,
            trace: 0xcbf2_9ce4_8422_2325,
            files: Vec::new(),
        }
    }

    /// Hash of every completion delivered so far (token, result, time). Two
    /// runs with the same seed and inputs produce the same value.
    pub fn trace_hash(&self) -> u64 {
        self.trace
    }

    fn range(&self, buf: BufId, off: u32, len: u32) -> core::ops::Range<usize> {
        self.limits.range(buf, off, len)
    }

    /// Set or remove the contents of a registered file, as an operator
    /// replacing a credentials file would. Test setup, so it may allocate.
    pub fn set_file(&mut self, file: FileId, contents: Option<&[u8]>) {
        self.files[file.0 as usize] = contents.map(|c| c.to_vec());
    }

    /// Bind a UDP socket for a simulated server. Not part of `Io`: production
    /// code never serves UDP.
    pub fn bind_udp(&mut self, sock: Sock, addr: SocketAddrV4) {
        let s = &mut self.socks[sock.0 as usize];
        s.state = SockState::UdpBound;
        s.addr = Some(addr);
        s.has_peer = false;
        s.rx_head = 0;
        s.rx_len = 0;
    }
}
