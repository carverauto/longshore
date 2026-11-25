// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A log and its replicas on one backend: a frame is received into frame
//! memory, written from there to a file with direct I/O and synced, and
//! sent from there to three peers. Its bytes are moved once, by the
//! receive, and its memory is free again only when the write, the sync and
//! all three sends are done.

use super::{fail, must, prefix, sizes, World, STORAGE};
use crate::{Addr, Conn, Event, Frame, Rx};
use longshore_common::io::extents::Extent;
use longshore_common::io::{Completion, FileSlot, Io, Limits, Op, OpenFlags, Token};

const FILE: FileSlot = FileSlot::new(0);
const BLOCK: u32 = 4096;
const OPEN: u64 = STORAGE;
const WRITE: u64 = STORAGE + 1;
const SYNC: u64 = STORAGE + 2;
const READ: u64 = STORAGE + 3;
/// Payload lengths; with the prefix the longest fills one block exactly.
const LENS: [u32; 4] = [4092, 100, 2000, 1];

/// Byte `at` of the payload of round `i`.
const fn pattern(i: u32, at: usize) -> u8 {
    (at as u8).wrapping_mul(31) ^ (i as u8)
}

/// Whether `frame` is round `i`'s: the prefix, then the pattern.
fn is_round(frame: &[u8], i: u32) -> bool {
    let len = LENS[i as usize % LENS.len()];
    frame.len() == len as usize + 4
        && frame[..4] == len.to_be_bytes()
        && frame[4..]
            .iter()
            .enumerate()
            .all(|(at, b)| *b == pattern(i, at))
}

/// What one round is waiting for.
#[derive(Default)]
struct Round {
    /// The frame at the log, once received, and its memory.
    frame: Option<(Frame, Extent)>,
    /// The extent the file is read back into.
    back: Option<Extent>,
    synced: bool,
    read_back: bool,
    /// Peers that have received the frame.
    arrived: u32,
}

/// A source connection, three peers and an open log file.
pub struct Replicated<IO: Io> {
    w: World<IO>,
    /// The client's end of the source connection.
    source: Conn,
    /// The server's end of the source connection.
    log: Conn,
    /// The server's ends of the peers.
    peers: [Conn; 3],
    file_off: u64,
}

impl<IO: Io> Replicated<IO> {
    /// Connect four times to a listener at `addr` and create the log at
    /// `path`. `make` builds a backend with room for one file.
    pub fn new(make: &mut impl FnMut(Limits) -> IO, addr: Addr, path: &str) -> Replicated<IO> {
        let base = Limits::BASE.with_files_max(1);
        let w = World::new(make, base, sizes(4, 0, 0), sizes(2, 4, 2));
        Replicated::over(w, addr, path)
    }

    /// The same in a world the caller built: four connections out, four
    /// in, a listener, and room for one file.
    pub fn over(mut w: World<IO>, addr: Addr, path: &str) -> Replicated<IO> {
        // Direct mode: a frame is received at the start of its own extent.
        let opts = prefix().with_rx(Rx::Direct);
        let (source, log) = w.pair(addr, opts);
        let to = w.client.peer(&w.io, source).map_or(addr, Addr::Ip);
        let peers = [(); 3].map(|()| w.connect(to, opts).1);
        // The path has to lie in the pool: an extent serves, then goes back.
        let scratch = must(w.server.extents().take().ok_or("none free"), "extent");
        let bytes = w.io.buf(scratch.buf());
        bytes[..path.len()].copy_from_slice(path.as_bytes());
        bytes[path.len()] = 0;
        let flags = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true)
            .with_empty(true)
            .with_direct(true);
        let open = Op::FileOpen {
            slot: FILE,
            path_buf: scratch.buf(),
            path_off: 0,
            path_len: path.len() as u32 + 1,
            flags,
        };
        must(w.io.submit(Token::new(OPEN), open), "open");
        let mut r = Replicated {
            w,
            source,
            log,
            peers,
            file_off: 0,
        };
        let done = r.wait_storage();
        if done.token().get() != OPEN || done.result() < 0 {
            fail(format_args!("opening {path}: {done:?}"));
        }
        if !must(r.w.server.extents().release(scratch), "scratch") {
            fail(format_args!("the scratch extent is still held"));
        }
        r
    }

    /// Turn until a storage completion is there.
    fn wait_storage(&mut self) -> Completion {
        for _ in 0..40_000 {
            if let Some(c) = self.w.next_storage() {
                return c;
            }
            self.w.turn();
        }
        fail(format_args!("no storage completion"))
    }

    /// One frame from the source: logged, synced, read back and replicated.
    /// Allocates nothing.
    pub fn round(&mut self, i: u32) {
        let len = LENS[i as usize % LENS.len()];
        let (client, io) = self.w.client();
        let sent = client.send_frame(io, self.source, len + 4, |to| {
            to[..4].copy_from_slice(&len.to_be_bytes());
            for (at, b) in to[4..].iter_mut().enumerate() {
                *b = pattern(i, at);
            }
        });
        must(sent, "send");
        let mut r = Round::default();
        for _ in 0..40_000 {
            while let Some(e) = self.w.client.next_event() {
                self.at_peer(e, i, &mut r);
            }
            while let Some(e) = self.w.server.next_event() {
                self.at_log(e, i, &mut r);
            }
            while let Some(c) = self.w.next_storage() {
                self.stored(c, i, &mut r);
            }
            if self.is_done(i, &r) {
                self.file_off += u64::from(BLOCK);
                return;
            }
            self.w.turn();
        }
        fail(format_args!("round {i} did not finish"));
    }

    /// The frame is whole while anything holds it, and its memory is free
    /// only when the sync and all three sends are done.
    fn is_done(&mut self, i: u32, r: &Round) -> bool {
        let Some((frame, ext)) = r.frame else {
            return false;
        };
        let (server, io) = self.w.server();
        let live = server.extents().is_live(ext);
        match server.bytes(io, &frame) {
            Some(bytes) if live && is_round(bytes, i) => false,
            Some(_) => fail(format_args!("round {i}: a held frame changed")),
            None if live => fail(format_args!("round {i}: a live frame has no bytes")),
            // Freed: the sync, which held it, must be done. The round ends
            // when the read back has returned its extent as well.
            None if !r.synced => fail(format_args!("round {i}: freed before the sync")),
            None => {
                let all_free = server.extents().free_count() == server.extents().count();
                r.read_back && r.arrived == 3 && all_free
            }
        }
    }

    /// An event of the server set: the frame arriving at the log.
    fn at_log(&mut self, e: Event, i: u32, r: &mut Round) {
        match e {
            Event::Readable(conn) if conn == self.log => {}
            Event::Writable(_) => return,
            Event::Readable(_)
            | Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Finished(_)
            | Event::Drained => fail(format_args!("round {i}, at the log: {e:?}")),
        }
        let (server, io) = self.w.server();
        let Some(frame) = must(server.next_frame(io, self.log), "next_frame") else {
            return;
        };
        let (span, held) = (frame.span(), frame.extent());
        let Some(ext) = held.filter(|_| span.off() == 0 && r.frame.is_none()) else {
            fail(format_args!(
                "round {i}: not one frame at the start of an extent"
            ));
        };
        // The file's hold, then the write of whole blocks from the frame.
        must(server.extents().hold(ext), "hold");
        let write = Op::FileWrite {
            slot: FILE,
            buf: span.buf(),
            off: 0,
            len: BLOCK,
            file_off: self.file_off,
        };
        must(io.submit(Token::new(WRITE), write), "write");
        for peer in self.peers {
            must(server.send_lent(peer, &frame), "send_lent");
        }
        // The caller's hold, the file's, and one for each send: no copies.
        if server.extents().holds(ext) != Some(5) {
            fail(format_args!(
                "round {i}: holds {:?}",
                server.extents().holds(ext)
            ));
        }
        server.release(frame);
        r.frame = Some((frame, ext));
    }

    /// A storage completion: the write, then the sync, then the read back.
    fn stored(&mut self, c: Completion, i: u32, r: &mut Round) {
        let Some((_, ext)) = r.frame else {
            fail(format_args!("round {i}: storage before the frame: {c:?}"));
        };
        let (server, io) = self.w.server();
        match (c.token().get(), c.result()) {
            (WRITE, 4096) => {
                let sync = Op::FileSync {
                    slot: FILE,
                    datasync: true,
                };
                must(io.submit(Token::new(SYNC), sync), "sync");
            }
            (SYNC, 0) => {
                r.synced = true;
                // Free now only if all three sends are done already.
                let _freed: bool = must(server.extents().release(ext), "the file's hold");
                let back = must(server.extents().take().ok_or("none free"), "extent");
                let read = Op::FileRead {
                    slot: FILE,
                    buf: back.buf(),
                    off: 0,
                    len: BLOCK,
                    file_off: self.file_off,
                };
                must(io.submit(Token::new(READ), read), "read");
                r.back = Some(back);
            }
            (READ, 4096) => {
                let Some(back) = r.back.take() else {
                    fail(format_args!("round {i}: a read nobody asked for"));
                };
                let len = LENS[i as usize % LENS.len()] as usize + 4;
                if !is_round(&io.buf(back.buf())[..len], i) {
                    fail(format_args!("round {i}: the file holds other bytes"));
                }
                if !must(server.extents().release(back), "read back") {
                    fail(format_args!(
                        "round {i}: the read-back extent is still held"
                    ));
                }
                r.read_back = true;
            }
            _ => fail(format_args!("round {i}: {c:?}")),
        }
    }

    /// An event of the client set: the frame arriving at a peer.
    fn at_peer(&mut self, e: Event, i: u32, r: &mut Round) {
        let conn = match e {
            Event::Readable(conn) if conn != self.source => conn,
            Event::Writable(_) => return,
            Event::Readable(_)
            | Event::Up(_)
            | Event::Accepted { .. }
            | Event::Refused { .. }
            | Event::Down { .. }
            | Event::Closed { .. }
            | Event::ListenFailed { .. }
            | Event::Finished(_)
            | Event::Drained => fail(format_args!("round {i}, at a peer: {e:?}")),
        };
        let (client, io) = self.w.client();
        while let Some(frame) = must(client.next_frame(io, conn), "next_frame") {
            if !client.bytes(io, &frame).is_some_and(|b| is_round(b, i)) {
                fail(format_args!("round {i}: a peer received other bytes"));
            }
            client.release(frame);
            r.arrived += 1;
        }
    }
}

/// Eight rounds on a backend from `make`, with the log at `path`.
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: Addr, path: &str) {
    let mut r = Replicated::new(make, addr, path);
    for i in 0..8 {
        r.round(i);
    }
}
