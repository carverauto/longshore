// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A frame pulled, wherever it lies: read, checked against the book, then
//! released, kept, or sent on. And what receiving in pages must keep
//! after every step, checked against the simulator's own count of the
//! pieces it has out, not against the sets' alone.

use super::book::read;
use super::drive::{Run, FRAME_MAX};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::{Io, Limits};
use longshore_tcp::{Frame, SendError};

/// Memory to read a frame in pages into, made before the run starts.
pub(crate) struct Paged {
    scratch: Vec<u8>,
    /// Bytes each set must have copied on receive, at least, for the
    /// frames in pages it delivered: each one's header and last block.
    copied: [u64; 2],
}

impl Paged {
    pub(crate) fn new() -> Paged {
        Paged {
            scratch: vec![0; FRAME_MAX],
            copied: [0; 2],
        }
    }
}

impl Run {
    /// The sender and number of `frame`, from its bytes: in one run of
    /// memory, or in pages. A frame in pages lies in whole pages of the
    /// area but for its last, partial block, which lies zero-padded in a
    /// block of its own.
    fn read_frame(&mut self, server: bool, frame: &Frame) -> Result<(u32, u32), String> {
        let Run { w, pages, .. } = self;
        let (net, io) = if server { w.server() } else { w.client() };
        let (first, count) = net.config().area();
        let in_area = |buf: longshore_common::io::BufId| {
            (first.get()..first.get() + count).contains(&buf.get())
        };
        let Some(spans) = net.pages(frame) else {
            let bytes = net
                .bytes(io, frame)
                .ok_or("a frame just pulled has no bytes")?;
            return read(bytes);
        };
        let (mut at, len) = (0usize, frame.len() as usize);
        for span in spans {
            let (off, n) = (span.off() as usize, span.len() as usize);
            let whole = at + n <= len - len % Limits::PAGE as usize;
            if whole && (!in_area(span.buf()) || off != 0 || n != Limits::PAGE as usize) {
                return Err(format!("a page of a frame in pages is {span:?}"));
            }
            let bytes = io.buf(span.buf());
            if !whole {
                // The tail block: its own, and zero beyond the frame.
                let pad = &bytes[off + n..off + Limits::PAGE as usize];
                if in_area(span.buf()) || pad.iter().any(|b| *b != 0) {
                    return Err(format!("the last block of a frame in pages is {span:?}"));
                }
            }
            pages.scratch[at..at + n].copy_from_slice(&bytes[off..off + n]);
            at += n;
        }
        if at != len {
            return Err(format!("{at} bytes in the pages of a frame of {len}"));
        }
        pages.copied[usize::from(server)] += 4 + (len % Limits::PAGE as usize) as u64;
        read(&pages.scratch[..len])
    }

    /// A frame pulled on handle `at`: checked, then released, kept, or
    /// sent on from where it lies.
    pub(crate) fn received(&mut self, server: bool, at: usize, frame: Frame) -> Result<(), String> {
        let side = usize::from(server);
        let (uid, seq) = self.read_frame(server, &frame)?;
        let on = self.sides[side].handles[at];
        self.note(format_args!(
            "   pulled on {:?} uid {}: frame {seq} of sender {uid}",
            on.conn, on.uid
        ));
        let sender = *self
            .senders
            .get(uid as usize)
            .ok_or_else(|| format!("no sender has the number {uid}"))?;
        self.stats.frames += 1;
        self.stats.protected_frames += u32::from(self.tls);
        self.stats.paged_frames += u32::from(frame.run().is_some());
        let h = &mut self.sides[side].handles[at];
        h.got += 1;
        let own = sender.server != server;
        if own {
            // The peer's own frame: always the same peer, and the next one.
            if *h.peer.get_or_insert(uid) != uid || seq != h.got_own {
                return Err(format!(
                    "frame {seq} of sender {uid}, after {} of sender {:?}",
                    h.got_own, h.peer
                ));
            }
            h.got_own += 1;
        }
        let (choice, room) = ((uid + seq) % 4, self.held_room(server));
        let (set, backend) = self.net(server);
        if choice == 0 || !own {
            sealed(|| set.release(frame));
            return Ok(());
        }
        match sealed(|| set.hold(backend, frame)) {
            Ok(held) if choice == 1 && room => self.sides[side].held.push(held),
            Ok(held) => self.relay(server, seq, held),
            // No frame memory to move it to: done with it as it is.
            Err(_) => sealed(|| set.release(frame)),
        }
        Ok(())
    }

    /// Send the held `frame` on to another connection of the same side,
    /// from where it lies, and release it.
    fn relay(&mut self, server: bool, pick: u32, frame: Frame) {
        if let Some(to) = self.pick(server, pick) {
            let target = self.sides[usize::from(server)].handles[to];
            let (net, _) = self.net(server);
            let open = sealed(|| net.can_send(target.conn));
            let got = sealed(|| net.send_lent(target.conn, &frame));
            self.note(format_args!(
                "   lent to {:?} uid {} (open: {open}) -> {got:?}",
                target.conn, target.uid
            ));
            if got == Ok(()) && open {
                self.senders[target.uid as usize].sent += 1;
                self.stats.relayed += 1;
            } else if got == Err(SendError::Full) {
                self.stats.full += 1;
                self.sides[usize::from(server)].handles[to].full = true;
            }
        }
        let (net, _) = self.net(server);
        sealed(|| net.release(frame));
    }

    /// What receiving in pages keeps between steps: the pieces the
    /// simulator has out are those the sets hold; the pages they lie in
    /// are no more than the sets keep, and their headers; no budget, no
    /// table is overrun; and each set copied at least what its frames
    /// say it must have.
    pub(crate) fn pages_hold(&mut self) -> Result<(), String> {
        let (mut pieces, mut pages) = (0, 0);
        for server in [false, true] {
            let (net, _) = self.net(server);
            let p = sealed(|| net.usage()).pages();
            let (kept, budget) = p.pages();
            if kept > budget || p.runs() > 3 {
                return Err(format!("over a budget (server: {server}): {p:?}"));
            }
            if p.moved_bytes() < self.pages.copied[usize::from(server)] {
                return Err(format!(
                    "copied {} bytes, and frames say {} at least",
                    p.moved_bytes(),
                    self.pages.copied[usize::from(server)]
                ));
            }
            (pieces, pages) = (pieces + p.pieces(), pages + kept + p.headers());
        }
        let out = self.w.io().pieces_out();
        if out.0 != pieces as usize || out.1 > pages as usize {
            return Err(format!(
                "the simulator has {out:?} pieces and pages out; the sets hold {pieces} pieces in {pages} pages at most"
            ));
        }
        Ok(())
    }

    /// At rest: no piece out, nothing kept.
    pub(crate) fn pages_at_rest(&mut self) -> Result<(), String> {
        self.pages_hold()?;
        for server in [false, true] {
            // The budget whole again, no run, record or header kept.
            let (net, _) = self.net(server);
            let p = sealed(|| net.usage()).pages();
            if (p.pages().0, p.runs(), p.pieces(), p.headers()) != (0, 0, 0, 0) {
                return Err(format!("kept at rest (server: {server}): {p:?}"));
            }
        }
        match self.w.io().pieces_out() {
            (0, 0) => Ok(()),
            out => Err(format!("{out:?} pieces and pages out at rest")),
        }
    }
}
