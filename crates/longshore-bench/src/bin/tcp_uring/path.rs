// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The peer of `tcp_uring path`: a log node. A block arrives from a
//! source, is written from where it lies to a preallocated file with
//! direct I/O and made durable, and only then is sent on to three
//! replicas and back to its source as the answer, all from the same
//! memory. The time the load measures is the whole path: wire, durable,
//! replicated, answered.
//!
//! With `--header` the block is the body of a frame, behind a header, as
//! in UltraGraphDB's bulk load. Direct I/O writes from a block boundary:
//! either the frame was received so that its body lies at one
//! (`--body-at block`), or the node moves the body to a second piece of
//! frame memory first and writes that (`--body-at frame`), the move the
//! other saves. What is sent on and back is the whole frame either way.

use super::load::ASKS;
use super::{thread_cpu, unexpected, Opts, Peer, Side, Stop, BLOCK};
use crate::die::{die, OrDie};
use longshore::io::extents::Extent;
use longshore::io::{FileSlot, Io, Op, OpenFlags, Token};
use longshore::tcp::{Conn, Event, Frame, SendError, Why};

const FILE: FileSlot = FileSlot::new(0);
/// Blocks of the file: it is written round and round.
const BLOCKS: u64 = 16_384;
/// The first token of the node's file operations, by what they are and
/// then by the slot of the source connection they are for.
const STORAGE: u64 = 1 << 41;
const OPEN: u64 = STORAGE;
const RESERVE: u64 = STORAGE + 1;
const WRITE: u64 = STORAGE + (1 << 20);
const SYNC: u64 = STORAGE + (2 << 20);
/// What the byte of a frame at `ASKS` says of it.
pub(super) const LOG: u8 = 2;
pub(super) const REPLICA: u8 = 3;

struct Node {
    /// The block each source connection has in flight, by its slot, and
    /// the memory its body was moved to, if it was.
    pending: Vec<Option<(Frame, Conn, Option<Extent>)>>,
    replicas: Vec<Conn>,
    next_block: u64,
    dsync: bool,
    /// Bytes of a frame, of the header in front of its block, and in
    /// front of the frame in its memory.
    frame: u32,
    header: u32,
    lead: u32,
    /// Whether the block lies at a block boundary as it was received.
    in_place: bool,
    seen: Peer,
}

/// Turn until the file operation `token` completes: its result.
fn wait<IO: Io>(side: &mut Side<IO>, token: u64) -> i32 {
    for _ in 0..100_000 {
        side.turn();
        while let Some(c) = side.others.pop_front() {
            if c.token().get() == token {
                return c.result();
            }
        }
    }
    die(format_args!("file operation {token} never completed"))
}

/// Create the log in the run's directory, empty, and reserve its blocks.
fn open<IO: Io>(side: &mut Side<IO>, o: &Opts) {
    let path = format!("{}/longshore-tcp-path-{}.dat", o.dir, std::process::id());
    let scratch = side
        .net
        .extents()
        .take()
        .or_die("frame memory for the path");
    let bytes = side.io.buf(scratch.buf());
    bytes[..path.len()].copy_from_slice(path.as_bytes());
    bytes[path.len()] = 0;
    let flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true)
        .with_empty(true)
        .with_direct(true)
        .with_dsync(o.dsync);
    let op = Op::FileOpen {
        slot: FILE,
        path_buf: scratch.buf(),
        path_off: 0,
        path_len: path.len() as u32 + 1,
        flags,
    };
    side.io.submit(Token::new(OPEN), op).ok().or_die("open");
    if wait(side, OPEN) < 0 {
        die(format_args!("cannot create {path} for direct I/O"));
    }
    let _freed = side.net.extents().release(scratch);
    let reserve = Op::FileReserve {
        slot: FILE,
        file_off: 0,
        len: BLOCKS * u64::from(BLOCK),
        keep_size: false,
    };
    side.io
        .submit(Token::new(RESERVE), reserve)
        .ok()
        .or_die("reserve");
    if wait(side, RESERVE) < 0 {
        die(format_args!("cannot reserve {path}"));
    }
    // Gone when the process ends; a leftover changes no result.
    let _: Option<()> = std::fs::remove_file(&path).ok();
}

/// Send `frame` to `conn` from where it lies. A connection that has gone
/// is passed over: at the end of a run the load closes its connections
/// while blocks are still becoming durable. A full queue is a fault of
/// the run's sizes.
fn forward<IO: Io>(side: &mut Side<IO>, conn: Conn, frame: &Frame) {
    match side.net.send_lent(conn, frame) {
        Ok(()) | Err(SendError::NotUp | SendError::Stale) => {}
        Err(e @ (SendError::Full | SendError::TooLarge | SendError::NotHeld)) => {
            die(format_args!("forwarding to {conn:?}: {e:?}"))
        }
    }
}

impl Node {
    /// A block arrived from `conn`: write it from where it lies, or
    /// from where it is moved to if it does not lie at a block boundary.
    fn log<IO: Io>(&mut self, side: &mut Side<IO>, conn: Conn, frame: Frame) {
        let Some(span) = frame.span() else {
            die(format_args!(
                "a frame in pages, which this path does not ask for"
            ));
        };
        let slot = conn.slot() as usize;
        if span.off() != self.lead || frame.len() != self.frame || self.pending[slot].is_some() {
            die(format_args!("not one frame where it was to lie, alone"));
        }
        let body = span.off() + self.header;
        // Each way is measured against the other: neither may be taken
        // for the other.
        if body.is_multiple_of(BLOCK) != self.in_place {
            die(format_args!("a body at {body} of its memory"));
        }
        let (buf, off, moved) = if self.in_place {
            (span.buf(), body, None)
        } else {
            let to = side
                .net
                .extents()
                .take()
                .or_die("frame memory to move a body to");
            let (from, into) = side.io.buf_pair(span.buf(), to.buf());
            let (start, len) = (body as usize, BLOCK as usize);
            into[..len].copy_from_slice(&from[start..start + len]);
            (to.buf(), 0, Some(to))
        };
        let write = Op::FileWrite {
            slot: FILE,
            buf,
            off,
            len: BLOCK,
            file_off: (self.next_block % BLOCKS) * u64::from(BLOCK),
        };
        self.next_block += 1;
        let token = Token::new(WRITE + slot as u64);
        side.io.submit(token, write).ok().or_die("write");
        self.pending[slot] = Some((frame, conn, moved));
    }

    /// A file operation completed: sync what was written, unless writes
    /// are durable by themselves; answer what is durable.
    fn stored<IO: Io>(&mut self, side: &mut Side<IO>, token: u64, result: i32) {
        let (written, slot) = match token {
            t if t >= SYNC => (false, (t - SYNC) as usize),
            t if t >= WRITE => (true, (t - WRITE) as usize),
            _ => return,
        };
        if result < 0 || (written && u32::try_from(result) != Ok(BLOCK)) {
            die(format_args!("file operation {token} ended with {result}"));
        }
        if written && !self.dsync {
            let sync = Op::FileSync {
                slot: FILE,
                datasync: true,
            };
            let asked = side.io.submit(Token::new(SYNC + slot as u64), sync);
            return asked.ok().or_die("sync");
        }
        let Some((frame, conn, moved)) = self.pending[slot].take() else {
            die(format_args!("a file operation for no block: {token}"));
        };
        if let Some(to) = moved {
            let _freed = side.net.extents().release(to);
        }
        // Durable: to the replicas and back to its source, from its memory.
        for replica in &self.replicas {
            forward(side, *replica, &frame);
        }
        forward(side, conn, &frame);
        side.net.release(frame);
        self.seen.frames += 1;
        self.seen.bytes += u64::from(self.frame);
    }
}

/// Log, replicate and answer, until told to stop.
pub(super) fn serve<IO: Io>(side: &mut Side<IO>, stop: &Stop, o: &Opts) -> Peer {
    open(side, o);
    let mut node = Node {
        pending: (0..o.conns + 3).map(|_| None).collect(),
        replicas: Vec::with_capacity(3),
        next_block: 0,
        dsync: o.dsync,
        frame: o.frame_len(),
        header: o.header,
        lead: o.framing(true).lead(),
        in_place: o.header == 0 || o.body_at_block,
        seen: Peer::default(),
    };
    let mut first = None;
    while !stop.now() {
        side.turn();
        while let Some(c) = side.others.pop_front() {
            node.stored(side, c.token().get(), c.result());
        }
        while let Some(e) = side.net.next_event() {
            let conn = match e {
                Event::Readable(conn) => conn,
                Event::Accepted { .. } | Event::Writable(_) => continue,
                Event::Closed { conn, why } => {
                    stop.closed();
                    if why != Why::Orderly {
                        // Not what a load does at its end: say so.
                        eprintln!("tcp_uring: the peer's {conn:?} closed: {why:?}");
                    }
                    continue;
                }
                Event::Up(_)
                | Event::Refused { .. }
                | Event::Finished(_)
                | Event::Down { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => unexpected(e),
            };
            while let Ok(Some(frame)) = side.net.next_frame(&mut side.io, conn) {
                first = first.or_else(|| Some((std::time::Instant::now(), thread_cpu())));
                let says = side
                    .net
                    .bytes(&mut side.io, &frame)
                    .and_then(|b| b.get(ASKS).copied());
                match says {
                    Some(LOG) => node.log(side, conn, frame),
                    Some(REPLICA) => {
                        node.replicas.push(conn);
                        side.net.release(frame);
                    }
                    _ => side.net.release(frame),
                }
            }
        }
    }
    if let Some((at, cpu0)) = first {
        (node.seen.secs, node.seen.cpu) = (at.elapsed().as_secs_f64(), thread_cpu() - cpu0);
    }
    node.seen
}
