// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The plan (C2): any sizes are refused as C2 says or planned as its table
//! says, and no file can take every slot.

use super::common::{between, sim};
use hegel::generators as gs;
use hegel::TestCase;
use longshore_common::io::{Limits, OpenFlags};
use longshore_file::check::{Rig, BLOCK};
use longshore_file::{ConfigError, FileConfig, FileError, FileSizes};

/// What C2 refuses `s` with, on small buffers of `small` bytes; empty
/// when it plans them.
fn refusals(s: (u32, u32, u32, u32, u32, u32), small: u32) -> Vec<ConfigError> {
    let (files, per_file, shared, _paths, steps, spans) = s;
    let mut why = Vec::new();
    if files == 0 || per_file == 0 || per_file > shared + 1 {
        why.push(ConfigError::Ops);
    }
    if steps == 0 || steps > per_file {
        why.push(ConfigError::Sequence);
    }
    if spans == 0 || spans > 1024 {
        why.push(ConfigError::Spans);
    }
    let path_max = small.saturating_sub(256) / 2;
    if u64::from(small) < u64::from(spans) * 12 || small < 256 || path_max < 2 {
        why.push(ConfigError::ClassSize);
    }
    why
}

#[hegel::test(derandomize = true, database = None::<String>)]
fn the_plan_refuses_or_adds_what_c2_says(tc: TestCase) {
    let s = (
        between(&tc, 0, 6),
        between(&tc, 0, 8),
        between(&tc, 0, 8),
        between(&tc, 0, 5),
        between(&tc, 0, 9),
        between(&tc, 0, 1100),
    );
    let (files, per_file, shared, paths, steps, spans) = s;
    let small = tc.draw(gs::sampled_from(vec![
        64u32, 256, 258, 300, 512, 1024, 13_000,
    ]));
    let base = Limits::BASE
        .with_small_buffers(between(&tc, 0, 3))
        .with_small_buffer_size(small)
        .with_files_max(between(&tc, 0, 3))
        .with_ops_max(between(&tc, 0, 10))
        .with_gather(between(&tc, 0, 2), between(&tc, 0, 2) * 600);
    let base = if base.gather_ops() == 0 || base.gather_spans() == 0 {
        base.with_gather(0, 0)
    } else {
        base
    };
    let token_base = u64::from(between(&tc, 0, 1000));
    let sizes = FileSizes::new(files, per_file)
        .with_shared(shared)
        .with_paths(paths)
        .with_sequence_max(steps)
        .with_spans_max(spans)
        .with_token_base(token_base);
    let why = refusals(s, small);
    match FileConfig::plan(base, sizes) {
        Err(e) => assert!(
            why.contains(&e),
            "{e:?} for {s:?}, small {small}; C2 says {why:?}"
        ),
        Ok((l, c)) => {
            assert!(
                why.is_empty(),
                "planned {s:?}, small {small}, though {why:?}"
            );
            let ops = files + paths + shared;
            assert_eq!(l.ops_max(), base.ops_max() + ops);
            assert_eq!(l.files_max(), base.files_max() + files);
            assert_eq!(l.gather_ops(), base.gather_ops() + ops);
            assert_eq!(l.gather_spans(), base.gather_spans().max(spans));
            assert_eq!(
                l.small_buffers(),
                base.small_buffers() + files + paths + ops
            );
            assert_eq!(l.small_buffer_size(), small);
            assert_eq!(c.path_max(), (small - 256) / 2 - 1);
            assert_eq!(c.token_base(), token_base);
            // A token for every operation slot and every file slot's own.
            assert_eq!(c.tokens(), u64::from(ops + files));
            // No file can take every slot: each other file keeps its own,
            // and the path operations theirs.
            assert!(per_file <= ops - (files - 1) - paths, "{s:?}");
        }
    }
}

/// On a set built from any plan that is taken: one file at its cap is
/// `Full`, and every other file still starts one operation, and every
/// path slot one path operation.
#[hegel::test(derandomize = true, database = None::<String>)]
fn no_file_takes_every_slot(tc: TestCase) {
    let files = between(&tc, 1, 4);
    let shared = between(&tc, 0, 4);
    let per_file = between(&tc, 1, shared + 1);
    let paths = between(&tc, 0, 3);
    let sizes = FileSizes::new(files, per_file)
        .with_shared(shared)
        .with_paths(paths)
        .with_sequence_max(1)
        .with_spans_max(4);
    let base = Limits::BASE
        .with_small_buffer_size(1024)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(BLOCK);
    let mut rig = Rig::new(&mut |l| sim(l, 512), base, sizes);
    let flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true);
    let all: Vec<_> = (0..files)
        .map(|i| rig.open(&format!("/f{i}"), flags))
        .collect();
    let busy = all[between(&tc, 0, files - 1) as usize];
    for _ in 0..per_file {
        let t = rig.call(|s, io, _| s.sync(io, busy, true));
        assert!(t.is_ok(), "within the cap: {t:?}");
    }
    rig.refused("at the cap", FileError::Full, |s, io, _| {
        s.sync(io, busy, true)
    });
    for f in all.iter().filter(|f| **f != busy) {
        let t = rig.call(|s, io, _| s.sync(io, *f, true));
        assert!(t.is_ok(), "another file's own slot: {t:?}");
    }
    for _ in 0..paths {
        let t = rig.call(|s, io, _| s.status(io, "/f0"));
        assert!(t.is_ok(), "a path slot: {t:?}");
    }
    rig.settle();
    let _done: Vec<_> = rig.take_events();
    for f in all {
        assert_eq!(rig.close(f), Ok(0));
    }
    rig.at_rest(0);
}
