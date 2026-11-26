// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs a list of steps against two sets on the simulator.

use super::book::{fill, Handle, Sender, Side, BODY_MIN};
use super::steps::{Kind, Step};
use super::world::{in_pages, opts, opts_with, world, Campaign, ACROSS_RECORDS, NAME};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::sim::SimIo;
use longshore_tcp::check::World;
use longshore_tcp::{
    Addr, Coverage, KernelTransport, ListenConfig, Net, NetError, PhaseCoverage, SendError,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub(crate) type SimNet = Net<KernelTransport<SimIo>>;

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)), 7000);
/// The longest frame, length prefix included: a send segment.
pub(crate) const FRAME_MAX: usize = 4096;
const LENGTHS: [usize; 5] = [BODY_MIN, 64, 700, 2000, FRAME_MAX - 4];
/// Frames the caller keeps at most, of the three each set has.
const HELD_MAX: usize = 2;

/// Where a run failed, and how.
#[derive(Debug)]
pub struct Failure {
    /// The step it failed at; the number of steps for the end of the run.
    pub step: usize,
    pub why: String,
}

/// What a run did, for a report of what the steps reached.
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub connects: u32,
    pub accepted: u32,
    pub refused: u32,
    pub frames: u32,
    /// Those of them that came over TLS.
    pub protected_frames: u32,
    /// Those of them that came in their pages.
    pub paged_frames: u32,
    pub relayed: u32,
    pub finished: u32,
    pub closed_orderly: u32,
    pub closed_otherwise: u32,
    pub went_down: u32,
    pub full: u32,
    pub stale_calls: u32,
    /// A hash of the run: the same for the same seed, every time.
    pub trace: u64,
    /// The cells of the state table both sets were in.
    pub cover: Coverage,
    /// And those of the TLS phase table of their transports.
    pub phases: PhaseCoverage,
}

pub(crate) struct Run {
    pub(crate) w: World<SimIo>,
    /// Its connections are protected with TLS.
    pub(crate) tls: bool,
    /// Bits are flipped in transit: a connection can end as a TLS failure.
    pub(crate) flips: bool,
    /// The run ends with a drain: whether `Drained` has been told.
    pub(crate) drained: Option<bool>,
    /// The client's side, then the server's.
    pub(crate) sides: [Side; 2],
    pub(crate) senders: Vec<Sender>,
    pub(crate) stats: Stats,
    /// Resets are among the faults: an end in order then says nothing of
    /// how much arrived.
    pub(crate) resets: bool,
    /// Print what each step did.
    pub(crate) trace: bool,
    /// Ends in order, to be judged when every sender's own end is known.
    pub(crate) ended: Vec<super::book::Ended>,
    /// Where frames in pages are read, and what they say was copied.
    pub(crate) pages: super::received::Paged,
}

/// Run `steps` with `seed` under the faults of `campaign`, then bring
/// everything to an end, printing what each step did
/// when `trace` is set.
pub fn run_in(
    campaign: Campaign,
    seed: u64,
    steps: &[Step],
    trace: bool,
) -> Result<Stats, Failure> {
    let mut run = Run {
        w: world(campaign, seed),
        tls: campaign.protected(seed),
        flips: campaign == Campaign::Flips,
        // Half the runs end with a drain of the server.
        drained: (seed / 2).is_multiple_of(2).then_some(false),
        sides: [Side::default(), Side::default()],
        senders: Vec::new(),
        stats: Stats::default(),
        resets: campaign.breaks(seed),
        trace,
        ended: Vec::new(),
        pages: super::received::Paged::new(),
    };
    for (at, step) in steps.iter().enumerate() {
        run.note(format_args!(
            "-- {at}: {:?} server={}",
            step.kind, step.server
        ));
        run.step(*step).map_err(|why| Failure { step: at, why })?;
        run.pages_hold().map_err(|why| Failure { step: at, why })?;
    }
    run.note(format_args!("-- the end"));
    let step = steps.len();
    run.finish().map_err(|why| Failure { step, why })?;
    run.pages_at_rest().map_err(|why| Failure { step, why })?;
    for server in [false, true] {
        let (net, _) = run.net(server);
        let (cover, phases) = (net.coverage(), net.transport().phase_coverage());
        run.stats.cover.add(&cover);
        run.stats.phases.add(&phases);
    }
    // Every completion the backend delivered, and what the run counted.
    let counted = u64::from(run.stats.frames) << 32 | u64::from(run.stats.accepted);
    run.stats.trace = run.w.io().trace_hash() ^ counted.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    Ok(run.stats)
}

impl Run {
    /// Say `what` happened, when tracing.
    pub(crate) fn note(&self, what: core::fmt::Arguments<'_>) {
        if self.trace {
            println!("{what}");
        }
    }

    pub(crate) fn net(&mut self, server: bool) -> (&mut SimNet, &mut SimIo) {
        if server {
            self.w.server()
        } else {
            self.w.client()
        }
    }

    fn step(&mut self, step: Step) -> Result<(), String> {
        let server = step.server;
        match step.kind {
            Kind::Turn => {
                sealed(|| self.w.turn());
                Ok(())
            }
            Kind::PullEvents => self.pull_events(server, 1 + step.a % 4),
            Kind::PullFrames => self.pull_frames(server, step.a, step.b % 4),
            Kind::Send => self.send(server, step.a, step.b),
            Kind::Connect => self.connect(step.a),
            Kind::Listen => self.listen(step.a),
            Kind::Close => self.close(server, step.a),
            Kind::CloseListener => self.close_listener(step.a),
            Kind::Shutdown => self.shutdown(server, step.a),
            Kind::Release => {
                self.release_one(server);
                Ok(())
            }
        }
    }

    /// A new handle of the caller's on `server`'s side.
    pub(crate) fn adopt(&mut self, server: bool, mut handle: Handle) -> Result<(), String> {
        let side = &self.sides[usize::from(server)];
        if side.find(handle.conn).is_some() {
            return Err(format!("{:?} was given out twice", handle.conn));
        }
        handle.uid = self.senders.len() as u32;
        self.senders.push(Sender {
            server,
            sent: 0,
            own: 0,
            shut: false,
            aborted: false,
            half_close: handle.half_close,
        });
        self.sides[usize::from(server)].handles.push(handle);
        Ok(())
    }

    fn connect(&mut self, bits: u32) -> Result<(), String> {
        // One connection in eight keeps its handle through going down.
        let reconnect = bits & 0x1c0 == 0x1c0;
        let (opts, half_close) = opts_with(bits, reconnect, self.tls)?;
        let tls = self.tls;
        let (net, io) = self.net(false);
        let dialled = sealed(|| {
            if tls {
                net.connect_tls(io, ADDR, NAME, opts)
            } else {
                net.connect(io, ADDR, opts)
            }
        });
        match dialled {
            Ok(conn) => {
                self.stats.connects += 1;
                let mut handle = Handle::new(conn, half_close, false, false);
                handle.reconnect = reconnect;
                self.adopt(false, handle)
            }
            Err(NetError::NoSlots) => Ok(()),
            Err(e) => Err(format!("connect: {e:?}")),
        }
    }

    fn listen(&mut self, bits: u32) -> Result<(), String> {
        let (opts, half_close) = opts(bits, self.tls)?;
        let limit = if bits & 4 == 0 { u32::MAX } else { 2 };
        // A port shared between rings is refused to the mode.
        let cfg = ListenConfig::new(opts)
            .with_reuse_port(!in_pages(bits, self.tls))
            .with_tls(self.tls)
            .with_limit(limit);
        let (net, io) = self.net(true);
        match sealed(|| net.listen(io, Addr::Ip(ADDR), cfg)) {
            Ok(l) => {
                let listeners = &mut self.sides[1].listeners;
                if listeners.iter().any(|(known, _, _)| *known == l) {
                    return Err(format!("{l:?} was given out twice"));
                }
                listeners.push((l, true, half_close));
                Ok(())
            }
            // Every slot may be closing still, or its address in use.
            Err(NetError::NoSlots | NetError::Listen(_)) => Ok(()),
            Err(e) => Err(format!("listen: {e:?}")),
        }
    }

    pub(crate) fn close_listener(&mut self, pick: u32) -> Result<(), String> {
        let count = self.sides[1].listeners.len();
        if count == 0 {
            return Ok(());
        }
        let at = pick as usize % count;
        let (l, open, _) = self.sides[1].listeners[at];
        let (net, io) = self.net(true);
        let got = sealed(|| net.close_listener(io, l));
        self.sides[1].listeners[at].1 = false;
        match (open, got) {
            (true, Ok(())) | (false, Err(NetError::Stale)) => Ok(()),
            (_, got) => Err(format!("close_listener of {l:?} (open: {open}): {got:?}")),
        }
    }

    fn close(&mut self, server: bool, pick: u32) -> Result<(), String> {
        match self.pick(server, pick) {
            Some(at) => self.close_at(server, at),
            None => Ok(()),
        }
    }

    /// Close the handle at `at` of the side's list.
    pub(crate) fn close_at(&mut self, server: bool, at: usize) -> Result<(), String> {
        let h = self.sides[usize::from(server)].handles[at];
        let (net, io) = self.net(server);
        let got = sealed(|| net.close(io, h.conn));
        self.note(format_args!(
            "   close {:?} uid {} -> {got:?}",
            h.conn, h.uid
        ));
        self.sides[usize::from(server)].handles[at].alive = false;
        self.senders[h.uid as usize].aborted = true;
        match (h.alive, got) {
            (true, Ok(())) | (false, Err(NetError::Stale)) => Ok(()),
            // A refused connection goes when it likes.
            (true, Err(NetError::Stale)) if h.refused => Ok(()),
            (_, got) => Err(format!(
                "close of {:?} (alive: {}): {got:?}",
                h.conn, h.alive
            )),
        }
    }

    fn shutdown(&mut self, server: bool, pick: u32) -> Result<(), String> {
        let Some(at) = self.pick(server, pick) else {
            return Ok(());
        };
        let h = self.sides[usize::from(server)].handles[at];
        let (net, io) = self.net(server);
        let got = sealed(|| net.shutdown_write(io, h.conn));
        self.note(format_args!(
            "   shutdown {:?} uid {} -> {got:?}",
            h.conn, h.uid
        ));
        match (h.alive, got) {
            (true, Ok(())) => {
                self.senders[h.uid as usize].shut = true;
                Ok(())
            }
            (true, Err(NetError::NotUp)) | (false, Err(NetError::Stale)) => Ok(()),
            (true, Err(NetError::Stale)) if h.refused => Ok(()),
            (_, got) => Err(format!(
                "shutdown of {:?} (alive: {}): {got:?}",
                h.conn, h.alive
            )),
        }
    }

    fn send(&mut self, server: bool, pick: u32, len: u32) -> Result<(), String> {
        let Some(at) = self.pick(server, pick) else {
            return Ok(());
        };
        let h = self.sides[usize::from(server)].handles[at];
        let sender = self.senders[h.uid as usize];
        let choices = LENGTHS.len() + usize::from(self.tls);
        let body = LENGTHS.get(len as usize % choices).copied();
        let total = 4 + body.unwrap_or(ACROSS_RECORDS);
        let (net, io) = self.net(server);
        let open = sealed(|| net.can_send(h.conn));
        let got =
            sealed(|| net.send_frame(io, h.conn, total as u32, |to| fill(to, h.uid, sender.own)));
        self.note(format_args!(
            "   send {:?} uid {} frame {} of {total} bytes -> {got:?}",
            h.conn, h.uid, sender.own
        ));
        match got {
            Ok(()) if open && !sender.shut => {
                let s = &mut self.senders[h.uid as usize];
                (s.sent, s.own) = (s.sent + 1, s.own + 1);
                // It has seen for itself that there is room: nothing is
                // owed for a refusal before this.
                self.sides[usize::from(server)].handles[at].full = false;
                Ok(())
            }
            Err(SendError::Full) if open => {
                self.stats.full += 1;
                self.sides[usize::from(server)].handles[at].full = true;
                Ok(())
            }
            Err(SendError::NotUp) if !open && (h.alive || h.refused) => Ok(()),
            Err(SendError::Stale) if !h.alive || h.refused => {
                self.stats.stale_calls += 1;
                Ok(())
            }
            got => Err(format!(
                "send on {:?} (alive: {}, can_send: {open}, shut: {}): {got:?}",
                h.conn, h.alive, sender.shut
            )),
        }
    }

    /// The handle `pick` names: most of the time one that is alive, and
    /// now and then any the side was ever given, ended ones included.
    pub(crate) fn pick(&self, server: bool, pick: u32) -> Option<usize> {
        let handles = &self.sides[usize::from(server)].handles;
        if handles.is_empty() {
            return None;
        }
        let alive = handles.iter().filter(|h| h.alive).count();
        if alive == 0 || pick.is_multiple_of(8) {
            return Some((pick / 8) as usize % handles.len());
        }
        let nth = (pick / 8) as usize % alive;
        handles
            .iter()
            .enumerate()
            .filter(|(_, h)| h.alive)
            .nth(nth)
            .map(|(at, _)| at)
    }

    pub(crate) fn release_one(&mut self, server: bool) {
        if let Some(frame) = self.sides[usize::from(server)].held.pop() {
            let (net, _) = self.net(server);
            sealed(|| net.release(frame));
        }
    }

    pub(crate) const fn held_room(&self, server: bool) -> bool {
        self.sides[server as usize].held.len() < HELD_MAX
    }
}
