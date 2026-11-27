// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A transfer in progress when its file is poisoned ends with every byte
//! the backend reported, not the point it would have resumed from
//! (contract C10).

use crate::rig::{disk, rw, Rig};
use longshore_common::io::{Completion, Io, Token};
use longshore_common::sim::{Faults, PlacedFault, SimIo};
use longshore_file::{ErrorKind, FileError, FileSizes, Ticket};

/// The backend token of ticket `t` of the rig's set.
fn token_of(r: &Rig, t: Ticket) -> Token {
    Token::new(r.files.config().token_base() + u64::from(t.index()))
}

/// Ticks until `n` completions have come, without handing them to the set.
fn collect(r: &mut Rig, n: usize) -> Vec<Completion> {
    let mut got = Vec::new();
    for _ in 0..100 {
        r.io.tick(false, &mut r.out).unwrap();
        while let Some(c) = r.out.pop() {
            got.push(c);
        }
        if got.len() == n {
            return got;
        }
    }
    panic!("{} of {n} completions came", got.len());
}

#[test]
fn a_direct_write_cut_inside_a_block_counts_its_bytes_when_a_sync_poisons_first() {
    let mut checked = 0;
    for seed in 0..20u64 {
        let mut r = Rig::new(FileSizes::new(1, 2));
        // 512-byte sectors: the cut can fall inside the block.
        *r.io = SimIo::new(r.limits, seed, Faults::NONE);
        r.io.setup_disk(disk(512));
        let f = r.open("/d", rw().with_direct(true));
        let e = r.extent(0x6D);
        r.io.place_disk_fault(2, PlacedFault::ShortWrite);
        r.io.place_disk_fault(3, PlacedFault::Fail);
        let w = r
            .files
            .write_at(&mut r.io, &mut r.x, f, (e, 0, 4096), 0)
            .unwrap();
        let s = r.files.sync(&mut r.io, f, false).unwrap();
        let mut done = collect(&mut r, 2);
        // The failed sync is taken first, in the same turn.
        let sync = token_of(&r, s);
        done.sort_by_key(|c| c.token() != sync);
        let cut = done[1].result();
        assert_eq!(done[1].token(), token_of(&r, w));
        for c in done {
            assert!(r.files.on_completion(&mut r.io, &mut r.x, c));
        }
        assert_eq!(r.wait(s).1, Err(FileError::SyncFailed { errno: 5 }));
        let got = r.wait(w).1;
        if cut <= 0 || cut >= 4096 {
            continue;
        }
        checked += 1;
        let want = FileError::Partial {
            done: u64::from(cut.unsigned_abs()),
            kind: ErrorKind::Poisoned,
        };
        assert_eq!(got, Err(want), "seed {seed}: the disk holds {cut} bytes");
    }
    assert!(checked > 0, "no seed cut the write inside its block");
}
