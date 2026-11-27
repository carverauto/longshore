// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The model test of `Files` (task 3.2): seeded random sequences of every
//! operation on the simulated disk, in six campaigns (no faults, reported
//! faults with faults placed on single operations, failed syncs, torn
//! writes with power loss, power loss alone, silent faults), with the
//! contract's invariants F1 to F10 checked after every step against a
//! model of what each file must hold, of tickets accepted and reported,
//! of holds on extents, and of what was synced.
//!
//! A failing run names its campaign and seed; it is replayed, printing
//! every step, with
//! `FILE_MODEL=<campaign>:<seed> cargo test -p longshore_file --test file_model replay -- --nocapture`;
//! `FILE_MODEL_SEEDS=<n>` runs `n` seeds a campaign.

/// Every call of the set and of the backend after the set is built runs
/// sealed under this: an allocation there aborts the run (F9).
#[global_allocator]
static ALLOC: longshore_common::io::alloc_guard::Guard = longshore_common::io::alloc_guard::Guard;

#[cfg(test)]
mod book;
#[cfg(test)]
mod checks;
#[cfg(test)]
mod events;
#[cfg(test)]
mod lives;
#[cfg(test)]
mod ops;
#[cfg(test)]
mod paths;
#[cfg(test)]
mod run;
#[cfg(test)]
mod seqs;
#[cfg(test)]
mod space;
#[cfg(test)]
mod state;
#[cfg(test)]
mod world;

#[cfg(test)]
mod campaigns {
    use super::run::{Run, Stats};
    use super::world::Campaign;

    /// Steps of a run.
    const STEPS: usize = 600;
    /// Seeds of a campaign: a thousand in release, fewer in debug.
    const SEEDS: u64 = if cfg!(debug_assertions) { 40 } else { 1000 };

    /// One step of a run, drawn from its generator.
    fn one(r: &mut Run) {
        if r.squeeze > 0 {
            r.squeeze -= 1;
            if r.squeeze == 0 {
                r.rig.io().set_room(None);
            }
        }
        if r.chance(r.campaign.power_losses()) {
            return r.power_loss();
        }
        match r.below(100) {
            0..=15 => r.write(),
            16..=22 => r.write_spans(),
            23..=36 => r.read(),
            37..=44 => r.sync(),
            45..=47 => r.space(false),
            48..=50 => r.space(true),
            51..=57 => r.sequence(),
            58..=62 => r.close(),
            63..=70 => r.open(),
            71..=73 => r.status(),
            74..=75 => r.path_op(0),
            76 => r.path_op(1),
            77 => r.path_op(2),
            78..=80 => r.dir_sync(),
            81..=95 => {
                for _ in 0..=r.below(3) {
                    r.turn();
                }
            }
            96 | 97 => r.use_stale(),
            98 => r.stale_extent(),
            _ => {
                // Squeeze the backend's room for a few steps: work waits.
                let room = r.below(3) as u32;
                r.rig.io().set_room(Some(room));
                r.squeeze = 1 + r.below(5) as u32;
            }
        }
    }

    /// Run `seed` of `campaign`; what it did.
    pub fn run(campaign: Campaign, seed: u64, trace: bool) -> Stats {
        let mut r = Run::new(campaign, seed, trace);
        for step in 0..STEPS {
            r.step = step;
            one(&mut r);
            r.check();
        }
        r.step = STEPS;
        r.finish();
        r.stats
    }

    /// Every seed of `campaign`; a failure names the seed to replay.
    fn campaign(campaign: Campaign) -> Stats {
        let mut total = Stats::default();
        // FILE_MODEL_SEEDS=<n> runs more (or fewer).
        let seeds = std::env::var("FILE_MODEL_SEEDS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(SEEDS);
        for seed in 0..seeds {
            let outcome = std::panic::catch_unwind(|| run(campaign, seed, false));
            match outcome {
                Ok(stats) => total.add(&stats),
                Err(_) => panic!(
                    "{campaign:?}, seed {seed} failed; replay with FILE_MODEL={campaign:?}:{seed}"
                ),
            }
        }
        println!("{campaign:?}: {total:?}");
        total
    }

    #[test]
    fn clean() {
        let t = campaign(Campaign::Clean);
        assert!(t.accepted > 1000 && t.read_bytes > 0, "{t:?}");
        assert!(t.full > 0 && t.waited > 0 && t.sequences > 0, "{t:?}");
        assert!(
            t.closes_held > 0 && t.refused > 0 && t.poisoned == 0,
            "{t:?}"
        );
    }

    #[test]
    fn reported_faults() {
        let t = campaign(Campaign::Reported);
        assert!(t.failed > 0 && t.cancelled > 0, "{t:?}");
    }

    #[test]
    fn failed_syncs() {
        let t = campaign(Campaign::SyncFails);
        assert!(t.poisoned > 0, "{t:?}");
    }

    #[test]
    fn torn_writes_and_power_loss() {
        let t = campaign(Campaign::Torn);
        assert!(t.failed > 0 && t.power_losses > 0, "{t:?}");
    }

    #[test]
    fn power_loss() {
        let t = campaign(Campaign::PowerLoss);
        assert!(t.power_losses > 0 && t.kept_bytes > 0, "{t:?}");
    }

    #[test]
    fn silent_faults() {
        let t = campaign(Campaign::Silent);
        assert!(t.power_losses > 0, "{t:?}");
    }

    /// `FILE_MODEL=<campaign>:<seed>` replays one run, printing each step.
    #[test]
    fn replay() {
        let Ok(spec) = std::env::var("FILE_MODEL") else {
            return;
        };
        let (name, seed) = spec.split_once(':').expect("FILE_MODEL=<campaign>:<seed>");
        let c = Campaign::named(name).expect("a campaign's name");
        let stats = run(c, seed.parse().expect("a seed"), true);
        println!("{stats:?}");
    }
}
