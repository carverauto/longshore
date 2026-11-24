// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the caller is told and what it pulls, checked against the book;
//! and the end of a run, where everything must come to rest.

use super::book::{read, Ended, Handle};
use super::drive::Run;
use longshore_tcp::{Conn, Event, Frame, Listener, NetError, SendError, Why};

impl Run {
    pub(crate) fn pull_events(&mut self, server: bool, most: u32) -> Result<(), String> {
        for _ in 0..most {
            let (net, _) = self.net(server);
            let Some(e) = net.next_event() else {
                return Ok(());
            };
            self.note(format_args!("   told (server={server}) {e:?}"));
            self.told(server, e)
                .map_err(|why| format!("{e:?}: {why}"))?;
        }
        Ok(())
    }

    /// The handle an event names, which must be one the caller holds.
    fn named(&self, server: bool, conn: Conn) -> Result<usize, String> {
        let side = &self.sides[usize::from(server)];
        let at = side
            .find(conn)
            .ok_or("a handle the caller was never given")?;
        let h = &side.handles[at];
        if h.alive {
            Ok(at)
        } else {
            Err("an event for a connection that has ended".to_string())
        }
    }

    fn told(&mut self, server: bool, e: Event) -> Result<(), String> {
        let side = usize::from(server);
        match e {
            Event::Up(conn) => {
                let at = self.named(server, conn)?;
                let h = &mut self.sides[side].handles[at];
                if core::mem::replace(&mut h.up, true) {
                    return Err("up twice".to_string());
                }
            }
            Event::Accepted { listener, conn } => {
                let half_close = self.accepting(server, listener)?;
                self.stats.accepted += 1;
                self.adopt(server, Handle::new(conn, half_close, true, false))?;
            }
            Event::Refused { listener, conn } => {
                let half_close = self.accepting(server, listener)?;
                self.stats.refused += 1;
                self.adopt(server, Handle::new(conn, half_close, true, true))?;
            }
            Event::Readable(conn) => {
                let _: usize = self.named(server, conn)?;
            }
            Event::Writable(conn) => {
                let at = self.named(server, conn)?;
                self.sides[side].handles[at].full = false;
            }
            Event::Finished(conn) => {
                let at = self.named(server, conn)?;
                let h = &mut self.sides[side].handles[at];
                if !h.half_close || core::mem::replace(&mut h.finished, true) {
                    return Err("finished without half-close, or twice".to_string());
                }
                self.stats.finished += 1;
                self.ended_in_order(server, at)?;
                let (net, io) = self.net(server);
                match net.next_frame(io, conn) {
                    Ok(None) => {}
                    got => return Err(format!("input left behind the end: {got:?}")),
                }
            }
            Event::Closed { conn, why } => {
                let at = self.named(server, conn)?;
                let h = &mut self.sides[side].handles[at];
                h.alive = false;
                if h.refused {
                    return Err("a refused connection was reported closed".to_string());
                }
                if why == Why::Orderly {
                    self.stats.closed_orderly += 1;
                    self.ended_in_order(server, at)?;
                } else {
                    // Whatever it had queued may not have gone.
                    self.stats.closed_otherwise += 1;
                    let uid = self.sides[side].handles[at].uid;
                    self.senders[uid as usize].aborted = true;
                }
            }
            Event::Down { .. } | Event::ListenFailed { .. } | Event::Drained => {
                return Err("an event nothing in this run asks for".to_string());
            }
        }
        Ok(())
    }

    /// Whether connections of `listener` are half-close; it must be a
    /// listener the caller was given, open or closed since.
    fn accepting(&self, server: bool, listener: Listener) -> Result<bool, String> {
        let known = &self.sides[usize::from(server)].listeners;
        let found = known.iter().find(|(l, _, _)| *l == listener);
        found
            .map(|(_, _, half_close)| *half_close)
            .ok_or_else(|| "a listener the caller was never given".to_string())
    }

    /// The stream of handle `at` ended in order: noted, and judged at the
    /// end of the run, when it is known how its sender itself ended.
    fn ended_in_order(&mut self, server: bool, at: usize) -> Result<(), String> {
        let h = self.sides[usize::from(server)].handles[at];
        if let Some(peer) = h.peer {
            self.ended.push(Ended {
                peer,
                receiver: h.uid,
                got: h.got,
                got_own: h.got_own,
            });
        }
        Ok(())
    }

    /// Every stream that ended in order carried everything its sender
    /// queued, if that sender shut its side down in order and nothing else
    /// ended it: it was not closed, did not fail, and did not, without
    /// half-close, end at its peer's finishing first.
    fn judge_ends(&self) -> Result<(), String> {
        for e in &self.ended {
            let peer = self.senders[e.peer as usize];
            let ours = self.senders[e.receiver as usize];
            let kept = peer.half_close || !(ours.shut || ours.aborted);
            let whole = peer.shut && !peer.aborted && !self.resets && kept;
            if whole && (e.got, e.got_own) != (peer.sent, peer.own) {
                return Err(format!(
                    "sender {} ended in order with {} of {} frames received ({} of {} of its own)",
                    e.peer, e.got, peer.sent, e.got_own, peer.own
                ));
            }
        }
        Ok(())
    }

    /// Pull up to `most` frames of the handle `pick` names (all of them
    /// for 0), checking each against the book.
    pub(crate) fn pull_frames(&mut self, server: bool, pick: u32, most: u32) -> Result<(), String> {
        match self.pick(server, pick) {
            Some(at) => self.pull_at(server, at, most),
            None => Ok(()),
        }
    }

    /// Pull up to `most` frames of handle `at` (all of them for 0).
    fn pull_at(&mut self, server: bool, at: usize, most: u32) -> Result<(), String> {
        let mut pulled = 0;
        while most == 0 || pulled < most {
            let h = self.sides[usize::from(server)].handles[at];
            let (net, io) = self.net(server);
            let frame = match (h.alive, net.next_frame(io, h.conn)) {
                (true, Ok(Some(frame))) => frame,
                (true, Ok(None)) | (false, Err(NetError::Stale)) => return Ok(()),
                (true, Err(NetError::Stale)) if h.refused => return Ok(()),
                (_, got) => {
                    return Err(format!(
                        "next_frame of {:?} (alive: {}): {got:?}",
                        h.conn, h.alive
                    ));
                }
            };
            pulled += 1;
            self.received(server, at, frame)
                .map_err(|why| format!("frame {} on {:?}: {why}", h.got, h.conn))?;
        }
        Ok(())
    }

    /// A frame pulled on handle `at`: checked, then released, kept, or
    /// sent on from where it lies.
    fn received(&mut self, server: bool, at: usize, frame: Frame) -> Result<(), String> {
        let side = usize::from(server);
        let (net, io) = self.net(server);
        let bytes = net
            .bytes(io, &frame)
            .ok_or("a frame just pulled has no bytes")?;
        let (uid, seq) = read(bytes)?;
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
        let choice = (uid + seq) % 4;
        let (set, backend) = self.net(server);
        if choice == 0 || !own {
            set.release(frame);
        } else if let Ok(held) = set.hold(backend, frame) {
            if choice == 1 && self.held_room(server) {
                self.sides[side].held.push(held);
            } else {
                self.relay(server, seq, held);
            }
        }
        Ok(())
    }

    /// Send the held `frame` on to another connection of the same side,
    /// from where it lies, and release it.
    fn relay(&mut self, server: bool, pick: u32, frame: Frame) {
        if let Some(to) = self.pick(server, pick) {
            let target = self.sides[usize::from(server)].handles[to];
            let (net, _) = self.net(server);
            let open = net.can_send(target.conn);
            let got = net.send_lent(target.conn, &frame);
            if got == Ok(()) && open {
                self.senders[target.uid as usize].sent += 1;
                self.stats.relayed += 1;
            } else if got == Err(SendError::Full) {
                self.stats.full += 1;
                self.sides[usize::from(server)].handles[to].full = true;
            }
        }
        let (net, _) = self.net(server);
        net.release(frame);
    }

    /// Bring the run to an end: everything is pulled and released, every
    /// connection shut down and every listener closed, until nothing is
    /// left. A connection that will not end is a stall.
    pub(crate) fn finish(&mut self) -> Result<(), String> {
        self.settle_wakes()?;
        for _ in 0..6000 {
            let mut alive = 0;
            for server in [false, true] {
                self.pull_events(server, u32::MAX)?;
                while !self.sides[usize::from(server)].held.is_empty() {
                    self.release_one(server);
                }
                for at in 0..self.sides[usize::from(server)].handles.len() {
                    alive += self.wind_down(server, at)?;
                }
            }
            for at in 0..self.sides[1].listeners.len() {
                if self.sides[1].listeners[at].1 {
                    self.close_listener(at as u32)?;
                }
            }
            if alive == 0 {
                return self.at_rest();
            }
            self.w.turn();
        }
        let stuck: Vec<_> = self
            .sides
            .iter()
            .flat_map(|s| s.handles.iter().filter(|h| h.alive))
            .collect();
        let why = format!("these never ended: {stuck:?}");
        let (client, _) = self.net(false);
        let at_client = client.usage();
        let (server, _) = self.net(true);
        let at_server = server.usage();
        Err(format!("{why}; client {at_client:?}; server {at_server:?}"))
    }

    /// Every connection that was refused a send for want of room is told
    /// when there is room, with no further call of its own: here both
    /// sides only pull, and turn, until each has been told or has ended.
    fn settle_wakes(&mut self) -> Result<(), String> {
        for _ in 0..3000 {
            let mut waiting = 0;
            for server in [false, true] {
                self.pull_events(server, u32::MAX)?;
                while !self.sides[usize::from(server)].held.is_empty() {
                    self.release_one(server);
                }
                for at in 0..self.sides[usize::from(server)].handles.len() {
                    let h = self.sides[usize::from(server)].handles[at];
                    if h.alive && !h.refused {
                        self.pull_at(server, at, 0)?;
                    }
                    let now = self.sides[usize::from(server)].handles[at];
                    waiting += u32::from(now.alive && now.full && !now.refused);
                }
            }
            if waiting == 0 {
                return Ok(());
            }
            self.w.turn();
        }
        let stuck: Vec<_> = self
            .sides
            .iter()
            .flat_map(|s| s.handles.iter().filter(|h| h.alive && h.full))
            .collect();
        Err(format!("never told there was room again: {stuck:?}"))
    }

    /// Pull what handle `at` has and shut its write side down; 1 while it
    /// is still alive. A refused connection, which ends without a word,
    /// and one that has not come up after a while are closed.
    fn wind_down(&mut self, server: bool, at: usize) -> Result<u32, String> {
        let h = self.sides[usize::from(server)].handles[at];
        if !h.alive {
            return Ok(0);
        }
        if h.refused {
            self.close(server, at as u32)?;
            return Ok(0);
        }
        self.pull_at(server, at, 0)?;
        if !self.sides[usize::from(server)].handles[at].alive {
            return Ok(0);
        }
        let (net, io) = self.net(server);
        match net.shutdown_write(io, h.conn) {
            Ok(()) => self.senders[h.uid as usize].shut = true,
            Err(NetError::NotUp) => {}
            Err(e) => return Err(format!("shutdown of {:?} at the end: {e:?}", h.conn)),
        }
        Ok(1)
    }

    /// With every connection ended and every listener closed, both sets
    /// hold nothing once what was in flight has landed.
    fn at_rest(&mut self) -> Result<(), String> {
        self.judge_ends()?;
        for _ in 0..400 {
            self.w.turn();
        }
        for server in [false, true] {
            let (net, _) = self.net(server);
            if let Some(e) = net.next_event() {
                return Err(format!("told {e:?} after everything had ended"));
            }
            let usage = net.usage();
            if !usage.is_idle() {
                return Err(format!("not idle at the end (server: {server}): {usage:?}"));
            }
        }
        Ok(())
    }
}
