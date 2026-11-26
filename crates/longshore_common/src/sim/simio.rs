// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Deterministic virtual world ([`Virtual`]) and its [`SimIo`] engine alias.

use crate::io::engine::{Attempt, Engine, World};
use crate::io::{
    count_result, BufId, FileId, Limits, Op, Sock, Wake, EAGAIN, EBADF, ECONNREFUSED, ECONNRESET,
    ENAMETOOLONG, ENFILE, ENOBUFS, ENOENT, EPIPE,
};
use crate::sim::prng::Prng;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod area;
mod disk;
mod faults;
pub use disk::{DiskConfig, DiskFaultCounters, DiskFaults, PlacedFault};
mod ops;
mod ops_armed;
mod ops_net;
mod world;

/// Fault probabilities, in parts per million, evaluated per attempt.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    pub(crate) connect_refused: u32,
    pub(crate) reset: u32,
    pub(crate) short_send: u32,
    pub(crate) partial_recv: u32,
    pub(crate) bit_flip: u32,
    pub(crate) delay: u32,
    pub(crate) udp_drop: u32,
    pub(crate) reorder: u32,
    pub(crate) copied: u32,
    pub(crate) ring_full: u32,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SockState {
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SimAddr {
    Ip(SocketAddr),
    Unix([u8; 108], u8),
}

pub(super) struct SimSock {
    pub(super) state: SockState,
    pub(super) addr: Option<SimAddr>,
    pub(super) peer: u32,
    /// Bytes in flight towards this socket.
    pub(super) rx: Box<[u8]>,
    pub(super) rx_head: usize,
    pub(super) rx_len: usize,
    /// For a socket accepted from an IP listener: the address the simulator
    /// gives its peer, the listener's and a port derived from the peer's slot.
    pub(super) accepted_from: Option<SocketAddr>,
    /// A listener that shares its address with others (`SO_REUSEPORT`).
    pub(super) reuse_port: bool,
    pub(super) peer_closed: bool,
    /// The peer shut down its write side: receives end once drained.
    pub(super) peer_wr_shut: bool,
    /// This side shut down its write side: sends fail.
    pub(super) wr_shut: bool,
    /// This side shut down its read side: receives end, arrivals are dropped.
    pub(super) rd_shut: bool,
    pub(super) reset: bool,
    /// For UDP: whether `peer` is valid.
    pub(super) has_peer: bool,
    /// What a connect on this socket completes with once accepted: the
    /// segment size of the route it took, or 0.
    pub(super) segment: u32,
}

/// Another address of a listener.
pub(super) struct Route {
    pub(super) addr: SocketAddr,
    pub(super) listener: SocketAddr,
    /// What connections to `addr` report as their segment size.
    pub(super) segment: u32,
    /// Connects to `addr` that reached the listener.
    pub(super) connects: u64,
}

/// Virtual time added per tick.
pub(super) const STEP_NS: u64 = 100_000;
/// Bytes a virtual socket holds before senders stall.
pub(super) const SOCK_BUF: usize = 64 * 1024;
/// Ticks `tick(wait = true)` spins with nothing completing before reporting a deadlock.
pub(super) const STALL_TICKS: u32 = 100_000;

pub(super) struct SimWaker(pub(super) Arc<AtomicBool>);

impl Wake for SimWaker {
    fn wake(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Deterministic virtual world driven by [`Engine`].
pub struct Virtual {
    pub(super) faults: Faults,
    pub(super) rng: Prng,
    pub(super) now: u64,
    pub(super) socks: Box<[SimSock]>,
    /// Registered files and their current contents; `None` means absent.
    pub(super) files: Vec<Option<Vec<u8>>>,
    /// Preallocated simulated disk for `FileSlot` file ops.
    pub(in crate::sim::simio) disk: Option<disk::SimDisk>,
    pub(super) disk_faults: DiskFaults,
    pub(in crate::sim::simio) placed_faults: Vec<disk::PlacedEntry>,
    pub(super) file_op_seq: u64,
    pub(super) disk_counters: DiskFaultCounters,
    /// Extra addresses of listeners.
    pub(super) routes: Vec<Route>,
    /// The path limit connects to routed addresses report with their size.
    pub(super) path_limit: u32,
    pub(super) ring_base: u32,
    pub(super) ring_free: Box<[u16]>,
    pub(super) ring_free_len: usize,
    pub(super) ring_in_free: Box<[bool]>,
    /// The receive area's pages and the pieces out of them.
    pub(super) area: area::Area,
    /// A receive found no room to report in, this tick.
    pub(super) ring_full: bool,
    pub(super) accept_start: u32,
    pub(super) accept_len: u32,
    pub(super) keep_timing: Option<crate::io::KeepAlive>,
    pub(super) no_delay: bool,
    pub(super) keep_alive: bool,
}

impl Virtual {
    /// A world with the slots `limits` asks for, all free, and no disk. `seed`
    /// decides every random choice; `faults` says how often the network
    /// misbehaves.
    pub fn new(limits: Limits, seed: u64, faults: Faults) -> Self {
        let sock = || SimSock {
            state: SockState::Free,
            addr: None,
            peer: 0,
            rx: vec![0u8; SOCK_BUF].into_boxed_slice(),
            rx_head: 0,
            rx_len: 0,
            accepted_from: None,
            reuse_port: false,
            peer_closed: false,
            peer_wr_shut: false,
            wr_shut: false,
            rd_shut: false,
            reset: false,
            has_peer: false,
            segment: 0,
        };
        let ring_count = limits.ring_buffers as usize;
        Self {
            faults,
            rng: Prng::new(seed),
            now: 0,
            socks: (0..limits.sockets_max).map(|_| sock()).collect(),
            files: Vec::new(),
            disk: None,
            disk_faults: DiskFaults::default(),
            placed_faults: Vec::with_capacity(64),
            file_op_seq: 0,
            disk_counters: DiskFaultCounters::default(),
            routes: Vec::new(),
            path_limit: 0,
            ring_base: limits.ring_base(),
            ring_free: (0..limits.ring_buffers as u16).rev().collect(),
            ring_free_len: ring_count,
            ring_in_free: vec![true; ring_count].into_boxed_slice(),
            area: area::Area::new(&limits, seed),
            ring_full: false,
            accept_start: limits.accept_range_start,
            accept_len: limits.accept_range_len,
            keep_timing: None,
            no_delay: false,
            keep_alive: false,
        }
    }

    pub(super) fn power_loss(&mut self) {
        for s in self.socks.iter_mut() {
            s.state = SockState::Free;
            s.addr = None;
            s.peer = 0;
            s.rx_head = 0;
            s.rx_len = 0;
            s.peer_closed = false;
            (s.peer_wr_shut, s.wr_shut, s.rd_shut) = (false, false, false);
            s.reset = false;
            s.has_peer = false;
            s.segment = 0;
        }
        let n = self.ring_free.len();
        for (i, slot) in self.ring_free.iter_mut().enumerate() {
            *slot = (n - 1 - i) as u16;
        }
        self.ring_free_len = n;
        self.ring_in_free.fill(true);
        self.area.reset();
        if let Some(disk) = self.disk.as_mut() {
            disk.power_loss(&mut self.rng);
        }
    }
}

/// Deterministic simulator built on [`Engine`] and [`Virtual`].
pub type SimIo = Engine<Virtual>;

impl SimIo {
    /// A simulator with the slots `limits` asks for and no disk. `seed` decides
    /// every random choice, so a run with the same seed repeats exactly;
    /// `faults` says how often the network misbehaves.
    pub fn new(limits: Limits, seed: u64, faults: Faults) -> SimIo {
        Engine::with_world(limits, Virtual::new(limits, seed, faults))
    }

    /// Pieces of the receive area that are out, and pages with a piece
    /// out, by the simulator's own count: for a test that does not trust
    /// the counters of the code it tests.
    pub fn pieces_out(&mut self) -> (usize, usize) {
        self.world_mut().area.out()
    }

    /// Preallocate the virtual disk tables. Test setup, so it allocates once.
    pub fn setup_disk(&mut self, cfg: DiskConfig) {
        let files_max = self.limits.files_max;
        self.world_mut().disk = Some(disk::SimDisk::new(cfg, files_max));
    }

    /// Replace the network fault rates given at construction, from now on:
    /// for a test that wants a clean set-up and faults after it.
    pub fn set_faults(&mut self, faults: Faults) {
        self.world_mut().faults = faults;
    }

    /// Set probabilistic disk fault rates in parts per million.
    pub fn set_disk_faults(&mut self, faults: DiskFaults) {
        self.world_mut().disk_faults = faults;
    }

    /// Place a fault on the `op_num`-th accepted file operation (1-based).
    /// Test setup, so it may allocate.
    pub fn place_disk_fault(&mut self, op_num: u64, fault: PlacedFault) {
        assert!(op_num > 0, "file op_num must be 1-based");
        let placed = &mut self.world_mut().placed_faults;
        if let Some(existing) = placed.iter_mut().find(|e| !e.used && e.op_num == op_num) {
            existing.fault = fault;
            return;
        }
        if let Some(reuse) = placed.iter_mut().find(|e| e.used) {
            *reuse = disk::PlacedEntry {
                op_num,
                fault,
                used: false,
            };
            return;
        }
        placed.push(disk::PlacedEntry {
            op_num,
            fault,
            used: false,
        });
    }

    /// Return counts of injected disk faults.
    pub fn disk_fault_counters(&self) -> DiskFaultCounters {
        self.world().disk_counters
    }

    /// Flip bit `bit` (`0..8`) at `file_off` on the medium itself (durable bytes
    /// and write log alike).
    pub fn corrupt_medium_bit(&mut self, path: &str, file_off: u64, bit: u8) -> Result<(), i32> {
        let Some(disk) = self.world_mut().disk.as_mut() else {
            return Err(-ENOENT);
        };
        disk.corrupt_medium_bit(path.as_bytes(), file_off, bit)
    }

    /// Simulate whole-machine power loss: drop all in-flight socket and file
    /// ops with no completions, free all sockets and file slots, clear timers
    /// and the waker flag, and settle unsynced disk writes and directory
    /// changes in order using the simulator's seed.
    pub fn power_loss(&mut self) {
        self.reset_for_power_loss();
        self.world_mut().power_loss();
    }

    /// Set or remove the contents of a registered file, as an operator
    /// replacing a credentials file would. Test setup, so it may allocate.
    pub fn set_file(&mut self, file: FileId, contents: Option<&[u8]>) {
        self.world_mut().files[file.0 as usize] = contents.map(|c| c.to_vec());
    }

    /// Give the listener at `listener` another address, `addr`, as a store
    /// with several front ends has. Test setup, so it may allocate.
    pub fn route(&mut self, addr: SocketAddr, listener: SocketAddr, segment_size: u32) {
        let routes = &mut self.world_mut().routes;
        routes.retain(|r| r.addr != addr);
        routes.push(Route {
            addr,
            listener,
            segment: segment_size,
            connects: 0,
        });
    }

    /// The largest segment the simulated path could carry, which connects
    /// to routed addresses report next to their own size. 0 is unknown.
    pub fn set_path_limit(&mut self, path_limit: u32) {
        self.world_mut().path_limit = path_limit;
    }

    /// Connects to the routed address `addr` that reached its listener.
    pub fn route_connects(&self, addr: SocketAddr) -> u64 {
        self.world()
            .routes
            .iter()
            .find(|r| r.addr == addr)
            .map_or(0, |r| r.connects)
    }

    /// Bind a UDP socket for a simulated server. Not part of `Io`: production
    /// code never serves UDP.
    pub fn bind_udp(&mut self, sock: Sock, addr: SocketAddr) {
        let s = &mut self.world_mut().socks[sock.0 as usize];
        s.state = SockState::UdpBound;
        s.addr = Some(SimAddr::Ip(addr));
        s.has_peer = false;
        s.rx_head = 0;
        s.rx_len = 0;
    }
}
