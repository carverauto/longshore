// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A log that answers only what is durable, under network faults and a
//! power loss at a seeded moment.
//!
//! A frame arrives, is written to the log from where it lies and synced,
//! and only then sent on to three peers from the same memory. Then the
//! power goes. What a peer has seen must be in the file; what was written
//! and not yet synced may be there, torn, or gone.

use super::sim_addr;
use longshore_common::io::extents::Extent;
use longshore_common::io::{Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore_common::sim::{DiskConfig, Faults, SimIo};
use longshore_tcp::check::{prefix, sizes, World, STORAGE};
use longshore_tcp::{Conn, Event, Frame, Rx};

const FILE: FileSlot = FileSlot::new(0);
/// The directory the log is in: a new file is there after a power loss
/// only when its directory was synced.
const DIR: FileSlot = FileSlot::new(1);
const BLOCK: u32 = 4096;
const OPEN: u64 = STORAGE;
const WRITE: u64 = STORAGE + 1;
const SYNC: u64 = STORAGE + 2;
const READ: u64 = STORAGE + 3;
const DIR_OPEN: u64 = STORAGE + 4;
const DIR_SYNC: u64 = STORAGE + 5;
const PATH: &str = "/durable.dat";
const ROUNDS: u32 = 12;

/// A simulator whose disk keeps writes that are not synced apart from
/// what is durable, so that a power loss can lose or tear them.
fn sim(limits: Limits, seed: u64, faults: Faults) -> SimIo {
    let mut io = SimIo::new(limits, seed, faults);
    io.setup_disk(
        DiskConfig::BASE
            .with_max_files(4)
            .with_max_dirs(4)
            .with_max_bytes_per_file(1 << 20)
            .with_max_name_len(256)
            .with_write_log_entries(32)
            .with_write_log_bytes(64 * 1024),
    );
    io
}

/// The frame of round `i`: a length prefix and a block's worth of pattern.
fn frame_of(i: u32) -> Vec<u8> {
    let len = BLOCK as usize - 4;
    let mut f = (len as u32).to_be_bytes().to_vec();
    f.extend((0..len).map(|at| (at as u8).wrapping_mul(29) ^ (i as u8).wrapping_add(7)));
    f
}

/// Open the log, creating and emptying it when `create` is set.
fn open(io: &mut SimIo, scratch: Extent, create: bool) {
    let bytes = io.buf(scratch.buf());
    bytes[..PATH.len()].copy_from_slice(PATH.as_bytes());
    bytes[PATH.len()] = 0;
    let flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(create)
        .with_empty(create)
        .with_direct(true);
    let op = Op::FileOpen {
        slot: FILE,
        path_buf: scratch.buf(),
        path_off: 0,
        path_len: PATH.len() as u32 + 1,
        flags,
    };
    io.submit(Token::new(OPEN), op).unwrap();
}

/// What the run saw before the power went.
struct Seen {
    /// Rounds a peer received, each from all the bytes of its frame.
    at_peers: Vec<u32>,
    /// Rounds whose sync completed.
    synced: Vec<u32>,
}

/// The frame of the round in flight at the log, with the file's hold.
struct Logging {
    round: u32,
    frame: Frame,
    ext: Extent,
}

struct Log {
    w: World<SimIo>,
    source: Conn,
    log: Conn,
    peers: [Conn; 3],
    /// Answer first and sync when the peers have it: what the check must
    /// catch.
    eager: bool,
    /// An eager log's write is done and its sync not yet asked for.
    unsynced: bool,
    logging: Option<Logging>,
    seen: Seen,
}

impl Log {
    fn new(seed: u64, faults: Faults, eager: bool) -> Log {
        let base = Limits::BASE.with_files_max(2);
        let mut w = World::new(
            &mut |l| sim(l, seed, faults),
            base,
            sizes(4, 0, 0),
            sizes(2, 4, 2),
        );
        let opts = prefix().with_rx(Rx::Direct);
        let (source, log) = w.pair(sim_addr(), opts);
        let peers = [(); 3].map(|()| w.connect(sim_addr(), opts).1);
        let scratch = w.server().0.extents().take().unwrap();
        open(w.io(), scratch, true);
        let mut l = Log {
            w,
            source,
            log,
            peers,
            eager,
            unsynced: false,
            logging: None,
            seen: Seen {
                at_peers: Vec::new(),
                synced: Vec::new(),
            },
        };
        let opened = l.storage();
        assert!(opened.0 == OPEN && opened.1 >= 0, "open: {opened:?}");
        l.sync_directory(scratch);
        assert_eq!(l.w.server().0.extents().release(scratch), Ok(true));
        l
    }

    /// Make the new file's name durable: open its directory and sync it.
    fn sync_directory(&mut self, scratch: Extent) {
        let io = self.w.io();
        io.buf(scratch.buf())[..2].copy_from_slice(b"/\0");
        let open = Op::FileOpen {
            slot: DIR,
            path_buf: scratch.buf(),
            path_off: 0,
            path_len: 2,
            flags: OpenFlags::NONE.with_read(true).with_directory(true),
        };
        io.submit(Token::new(DIR_OPEN), open).unwrap();
        let opened = self.storage();
        assert!(
            opened.0 == DIR_OPEN && opened.1 >= 0,
            "directory: {opened:?}"
        );
        let sync = Op::FileSync {
            slot: DIR,
            datasync: false,
        };
        self.w.io().submit(Token::new(DIR_SYNC), sync).unwrap();
        assert_eq!(self.storage(), (DIR_SYNC, 0));
    }

    /// Turn until a storage completion: its token and result.
    fn storage(&mut self) -> (u64, i32) {
        for _ in 0..40_000 {
            if let Some(c) = self.w.next_storage() {
                return (c.token().get(), c.result());
            }
            self.w.turn();
        }
        panic!("no storage completion");
    }

    /// Send the frame to the three peers, from where it lies.
    fn forward(&mut self, frame: &Frame) {
        let (server, _) = self.w.server();
        for peer in self.peers {
            server.send_lent(peer, frame).unwrap();
        }
    }

    /// The frame of `round` arrived at the log: write it from its memory.
    fn arrived(&mut self, round: u32) {
        let (server, io) = self.w.server();
        let Some(frame) = server.next_frame(io, self.log).unwrap() else {
            return;
        };
        assert!(self.logging.is_none(), "round {round}: two frames at once");
        let (span, ext) = (frame.span(), frame.extent().unwrap());
        assert_eq!(
            span.off(),
            0,
            "round {round}: not at the start of an extent"
        );
        assert!(server.bytes(io, &frame).unwrap() == frame_of(round));
        server.extents().hold(ext).unwrap();
        let write = Op::FileWrite {
            slot: FILE,
            buf: span.buf(),
            off: 0,
            len: BLOCK,
            file_off: u64::from(round) * u64::from(BLOCK),
        };
        io.submit(Token::new(WRITE), write).unwrap();
        if self.eager {
            self.forward(&frame);
        }
        self.logging = Some(Logging { round, frame, ext });
    }

    /// A storage completion of the round in flight.
    fn stored(&mut self, token: u64, result: i32) {
        let Some(Logging { round, frame, ext }) = self.logging.take() else {
            panic!("storage with no frame: {token} {result}");
        };
        match (token, result) {
            (WRITE, 4096) => {
                self.logging = Some(Logging { round, frame, ext });
                if self.eager {
                    self.unsynced = true;
                } else {
                    self.sync();
                }
            }
            (SYNC, 0) => {
                self.seen.synced.push(round);
                // Durable: now it may be answered.
                if !self.eager {
                    self.forward(&frame);
                }
                let (server, io) = self.w.server();
                // Its memory is whole until the last hold goes: the
                // file's, the caller's, and the three sends'.
                assert!(server.bytes(io, &frame).unwrap() == frame_of(round));
                assert!(server.extents().holds(ext) >= Some(2), "round {round}");
                let _freed: bool = server.extents().release(ext).unwrap();
                server.release(frame);
            }
            other => panic!("round {round}: {other:?}"),
        }
    }

    fn sync(&mut self) {
        let sync = Op::FileSync {
            slot: FILE,
            datasync: true,
        };
        self.w.io().submit(Token::new(SYNC), sync).unwrap();
    }

    /// One turn: what arrived at the log and at the peers, and storage.
    fn step(&mut self, round: u32) {
        while let Some(e) = self.w.server().0.next_event() {
            match e {
                Event::Readable(conn) if conn == self.log => self.arrived(round),
                Event::Writable(_) => {}
                Event::Readable(_)
                | Event::Up(_)
                | Event::Accepted { .. }
                | Event::Refused { .. }
                | Event::Finished(_)
                | Event::Down { .. }
                | Event::Closed { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => panic!("at the log: {e:?}"),
            }
        }
        while let Some(e) = self.w.client().0.next_event() {
            let Event::Readable(conn) = e else {
                assert!(matches!(e, Event::Writable(_)), "at a peer: {e:?}");
                continue;
            };
            let (client, io) = self.w.client();
            while let Some(frame) = client.next_frame(io, conn).unwrap() {
                let bytes = client.bytes(io, &frame).unwrap();
                let which = (0..ROUNDS).find(|i| bytes == frame_of(*i));
                self.seen
                    .at_peers
                    .push(which.expect("a peer received other bytes"));
                client.release(frame);
            }
        }
        while let Some(c) = self.w.next_storage() {
            self.stored(c.token().get(), c.result());
        }
        let at_peers = self.seen.at_peers.iter().filter(|r| **r == round).count();
        if self.unsynced && at_peers == 3 {
            self.unsynced = false;
            self.sync();
        }
        self.w.turn();
    }

    /// Whether `round` is over: synced, at all three peers, memory free.
    fn settled(&mut self, round: u32) -> bool {
        let at_peers = self.seen.at_peers.iter().filter(|r| **r == round).count();
        let (server, _) = self.w.server();
        let free = server.extents().free_count() == server.extents().count();
        self.logging.is_none() && at_peers == 3 && free
    }

    /// Run until `cut` turns have passed, then lose power.
    fn until_power_loss(mut self, cut: u32) -> (SimIo, Seen, Extent) {
        let mut turns = 0;
        'rounds: for round in 0..ROUNDS {
            let (client, io) = self.w.client();
            client
                .send_bytes(io, self.source, &frame_of(round))
                .unwrap();
            for _ in 0..40_000 {
                if turns == cut {
                    break 'rounds;
                }
                turns += 1;
                self.step(round);
                if self.settled(round) {
                    continue 'rounds;
                }
            }
            panic!("round {round} did not finish");
        }
        // Memory for what is read back after the power is on again.
        let (server, _) = self.w.server();
        let back = match &self.logging {
            Some(l) => l.ext,
            None => server.extents().take().unwrap(),
        };
        let Log { w, seen, .. } = self;
        let mut io = w.into_io();
        io.power_loss();
        (io, seen, back)
    }
}

/// The block of `round` in the log after the power is on again.
fn read_back(io: &mut SimIo, back: Extent, round: u32) -> Vec<u8> {
    let read = Op::FileRead {
        slot: FILE,
        buf: back.buf(),
        off: 0,
        len: BLOCK,
        file_off: u64::from(round) * u64::from(BLOCK),
    };
    io.submit(Token::new(READ), read).unwrap();
    let got = wait(io, READ);
    let got = usize::try_from(got).unwrap_or(0);
    io.buf(back.buf())[..got].to_vec()
}

/// Tick the bare backend until `token` completes: its result.
fn wait(io: &mut SimIo, token: u64) -> i32 {
    let mut out = Completions::new(8);
    for _ in 0..1000 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == token {
                return c.result();
            }
        }
    }
    panic!("token {token} never completed");
}

/// Rounds a peer saw whose frame the log does not hold after the power
/// loss of the run of `seed`; and how many rounds were synced in all.
fn lost(seed: u64, eager: bool) -> (Vec<u32>, usize) {
    let faults = Faults::NONE
        .with_partial_recv(300_000)
        .with_short_send(300_000)
        .with_delay(200_000);
    // The power goes at any turn of the first rounds, or after the last.
    let cut = (seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40) as u32 % 700;
    let (mut io, seen, back) = Log::new(seed, faults, eager).until_power_loss(cut);
    open(&mut io, back, false);
    assert!(wait(&mut io, OPEN) >= 0, "seed {seed}: the log is gone");
    let mut missing = Vec::new();
    for round in 0..ROUNDS {
        let durable = read_back(&mut io, back, round) == frame_of(round);
        let answered = seen.at_peers.contains(&round);
        if answered && !durable {
            missing.push(round);
        }
        // And what was synced is there, answered or not.
        assert!(
            durable || !seen.synced.contains(&round),
            "seed {seed}: round {round} was synced and is not in the log"
        );
    }
    (missing, seen.synced.len())
}

#[test]
fn nothing_a_peer_saw_is_missing_from_the_log_after_a_power_loss() {
    let mut synced = 0;
    for seed in 0..1000 {
        let (missing, rounds) = lost(seed, false);
        assert!(
            missing.is_empty(),
            "seed {seed}: answered and lost: {missing:?}"
        );
        synced += rounds;
    }
    // The power went in the middle of the work, not before or after it.
    assert!(synced > 1000 && synced < 11_000, "{synced}");
}

#[test]
fn a_log_that_answers_before_its_sync_is_caught() {
    let caught = (0..300)
        .filter(|seed| !lost(*seed, true).0.is_empty())
        .count();
    assert!(caught > 0, "no run lost what it had answered");
}
