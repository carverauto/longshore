// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One run: a seed's steps against a set on the simulated disk, every
//! call of the set and of the backend under the allocation guard (F9),
//! the events judged as they come, the invariants after every step.

use super::state::{Room, State};
use super::world::{rig, Campaign};
use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::extents::{Extent, Extents};
use longshore_common::sim::prng::Prng;
use longshore_common::sim::SimIo;
use longshore_file::check::{Rig, Spy};
use longshore_file::{FileError, Files, Ticket};

/// What a run did, for the campaign's report of what it reached.
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub accepted: u64,
    pub refused: u64,
    pub full: u64,
    pub waited: u64,
    pub failed: u64,
    pub poisoned: u64,
    pub cancelled: u64,
    pub sequences: u64,
    pub power_losses: u64,
    pub read_bytes: u64,
    pub kept_bytes: u64,
    pub closes_held: u64,
}

impl Stats {
    pub fn add(&mut self, o: &Stats) {
        self.accepted += o.accepted;
        self.refused += o.refused;
        self.full += o.full;
        self.waited += o.waited;
        self.failed += o.failed;
        self.poisoned += o.poisoned;
        self.cancelled += o.cancelled;
        self.sequences += o.sequences;
        self.power_losses += o.power_losses;
        self.read_bytes += o.read_bytes;
        self.kept_bytes += o.kept_bytes;
        self.closes_held += o.closes_held;
    }
}

pub struct Run {
    pub rig: Rig<SimIo>,
    pub st: State,
    pub rng: Prng,
    pub campaign: Campaign,
    pub step: usize,
    /// Turns of the backend so far.
    pub turns: usize,
    pub trace: bool,
    pub stats: Stats,
    /// The first submission not yet checked (F1, F6).
    pub mark: u64,
    /// Tickets that ended with a power loss: none may come again (C12).
    pub lost: Vec<Ticket>,
    /// Files an event of this turn said were poisoned.
    pub poison_due: Vec<longshore_file::File>,
    /// Steps left with the backend's room squeezed.
    pub squeeze: u32,
    /// Pages of the receive area a write in flight is from.
    pub pages_busy: [bool; super::world::PAGES as usize],
}

impl Run {
    pub fn new(campaign: Campaign, seed: u64, trace: bool) -> Run {
        Run {
            rig: rig(campaign, seed),
            st: State::new(),
            rng: Prng::new(seed ^ 0x5eed_f11e),
            campaign,
            step: 0,
            turns: 0,
            trace,
            stats: Stats::default(),
            mark: 0,
            squeeze: 0,
            poison_due: Vec::new(),
            lost: Vec::new(),
            pages_busy: [false; super::world::PAGES as usize],
        }
    }

    pub fn note(&self, what: core::fmt::Arguments<'_>) {
        if self.trace {
            println!("{:>5} {what}", self.step);
        }
    }

    pub fn chance(&mut self, per_million: u32) -> bool {
        self.rng.chance(per_million)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.rng.below(n)
    }

    /// Turn the backend once, sealed, and judge every event it brought.
    pub fn turn(&mut self) {
        self.turns += 1;
        sealed(|| self.rig.tick());
        // One completion at a time: what the set submits while it takes
        // one is held to the state its events leave (F1, F6).
        while sealed(|| self.rig.complete_one()) {
            self.take_events();
            self.submitted_rightly();
        }
        sealed(|| self.rig.poll());
        self.take_events();
        self.submitted_rightly();
        if self.rig.usage().waiting() > 0 {
            self.stats.waited += 1;
        }
    }

    /// Judge every event the set has.
    fn take_events(&mut self) {
        while let Some((_, e)) = self.rig.pop_event() {
            self.on_event(e);
        }
        self.poison_seen();
    }

    /// The caller's extents and their holds now.
    pub fn holds(&mut self) -> Vec<(Extent, Option<u16>)> {
        let ours: Vec<Extent> = self.st.ours.iter().map(|(e, _)| *e).collect();
        ours.into_iter()
            .map(|e| (e, self.rig.x().holds(e)))
            .collect()
    }

    /// Turn until nothing is in flight, with the backend's room back.
    pub fn settle(&mut self) {
        self.rig.io().set_room(None);
        self.squeeze = 0;
        for _ in 0..20_000 {
            if self.st.pending.is_empty() {
                return;
            }
            self.turn();
        }
        panic!("never settled: {} in flight", self.st.pending.len());
    }

    /// A free extent, with the caller's hold.
    pub fn take(&mut self) -> Option<Extent> {
        let e = self.rig.x().take()?;
        self.st.hold(e, true);
        Some(e)
    }

    /// Let go of the caller's hold on `e`.
    pub fn release(&mut self, e: Extent) {
        let _freed: bool = self.rig.x().release(e).unwrap();
        self.st.hold(e, false);
    }

    /// Ask the set for one operation, sealed, and hold the answer against
    /// what the model says of it: refused with one of `why` (or `Full`
    /// where `room` allows), taken only if `why` is empty and there is
    /// room. A refusal submits nothing and takes nothing (C11).
    pub fn ask(
        &mut self,
        what: &str,
        why: &[FileError],
        room: Room,
        call: impl FnOnce(&mut Files, &mut Spy<SimIo>, &mut Extents) -> Result<Ticket, FileError>,
    ) -> Option<Ticket> {
        let before = (self.rig.usage(), self.rig.io().submitted(), self.holds());
        let got = sealed(|| self.rig.call(call));
        self.judge(what, got.map(|_| ()), why, room, &before);
        self.submitted_rightly();
        got.ok()
    }

    /// Whether `got` is what `why` and `room` allow; on a refusal, that
    /// nothing changed since `before`: no slot, no submission, and no hold
    /// on any of the caller's extents, the ones the call named among them.
    pub fn judge(
        &mut self,
        what: &str,
        got: Result<(), FileError>,
        why: &[FileError],
        room: Room,
        before: &(longshore_file::FileUsage, u64, Vec<(Extent, Option<u16>)>),
    ) {
        let (sent, waiting) = (self.rig.io().submitted(), self.rig.usage().waiting());
        self.note(format_args!(
            "{what}: {got:?} (allowed {why:?}, room {room:?}; {sent} sent, {waiting} waiting)"
        ));
        match got {
            Ok(()) => {
                assert!(
                    why.is_empty() && room != Room::Never,
                    "{what} taken though {why:?}, room {room:?}"
                );
                self.stats.accepted += 1;
            }
            Err(e) => {
                let full_ok = e == FileError::Full && room != Room::Must;
                assert!(
                    why.contains(&e) || full_ok,
                    "{what} refused {e:?}; allowed {why:?}, room {room:?}"
                );
                let after = (self.rig.usage(), self.rig.io().submitted(), self.holds());
                assert_eq!(&after, before, "{what}: the refusal changed the set");
                self.stats.refused += 1;
                if e == FileError::Full {
                    self.stats.full += 1;
                }
            }
        }
    }

    /// Place a fault on the next operation the set submits, at the
    /// campaign's rate, when the call is sure to be taken and to go at
    /// once.
    pub fn maybe_place(&mut self, sure: bool) {
        use longshore_common::sim::PlacedFault;
        let rate = self.campaign.placed();
        let idle = self.squeeze == 0 && self.rig.usage().waiting() == 0;
        if !sure || rate == 0 || !idle || !self.chance(rate) {
            return;
        }
        let fault = match self.below(5) {
            0 => PlacedFault::Fail,
            1 => PlacedFault::FailAfter,
            2 => PlacedFault::ShortWrite,
            3 => PlacedFault::TornReported,
            _ => PlacedFault::LostReported,
        };
        let n = self.rig.io().submitted() + 1;
        self.note(format_args!("  {fault:?} placed on submission {n}"));
        self.rig.io().inner_mut().place_disk_fault(n, fault);
    }

    /// A ticket just taken is none in flight and none lost with the power.
    pub fn fresh(&self, t: Ticket) {
        assert!(
            !self.lost.contains(&t),
            "C12: {t:?} ended with the power and came again"
        );
        let twice = self.st.pending.iter().any(|p| p.ticket == t);
        assert!(!twice, "F2: {t:?} is in flight already");
    }

    /// Bytes of `len`, made from the run's generator.
    pub fn data(&mut self, len: usize) -> Vec<u8> {
        let seed = self.rng.next_u64();
        (0..len)
            .map(|i| (seed >> ((i % 8) * 8)) as u8 ^ (i as u8).wrapping_mul(13))
            .collect()
    }
}
