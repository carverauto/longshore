// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving in pages (`Rx::ZeroCopy`): what the plan asks and refuses.

use super::sizes;
use longshore_common::io::Limits;
use longshore_tcp::{Config, ConfigError};

#[test]
fn pages_need_a_frame_buffer_for_every_connection() {
    // Six connections, five frame buffers.
    let short = sizes(3, 3, 1).with_frames(5, 8192).with_zc(64, 8, 32);
    assert_eq!(
        Config::plan(Limits::BASE, short).map(|_| ()),
        Err(ConfigError::Frames { have: 5, need: 6 })
    );
    let enough = short.with_frames(6, 8192);
    assert!(Config::plan(Limits::BASE, enough).is_ok());
    // Without pages the rule does not apply: direct mode waits as before.
    let none = sizes(3, 3, 1).with_frames(5, 8192);
    assert!(Config::plan(Limits::BASE, none).is_ok());
}

#[test]
fn pages_need_runs_and_piece_records_and_fit_the_returns() {
    let base = sizes(2, 2, 1);
    for (frames, pieces) in [(0, 32), (8, 0), (8, 40_000)] {
        let s = base.with_zc(64, frames, pieces);
        assert_eq!(
            Config::plan(Limits::BASE, s).map(|_| ()),
            Err(ConfigError::Pages),
            "{frames} runs, {pieces} records"
        );
    }
}

#[test]
fn the_plan_asks_for_an_area_and_a_cancel_a_connection() {
    let plain = sizes(2, 2, 1);
    let paged = plain.with_zc(64, 8, 32).with_zc_spare(10);
    let (without, _) = Config::plan(Limits::BASE, plain).unwrap();
    let (with, cfg) = Config::plan(Limits::BASE, paged).unwrap();
    assert_eq!(without.area_pages(), 0);
    assert_eq!(with.ops_max(), without.ops_max() + 4);
    // The budget, a header a connection, a turn's completions, the spare.
    let a_turn = 2 * with.ops_max() + 2;
    assert_eq!(with.area_pages(), 64 + 4 + a_turn + 10);
    // Eight tail blocks, two to a frame buffer of 8 KiB.
    assert_eq!(with.aligned_buffers(), without.aligned_buffers() + 4);
    let cfg = cfg.resolved(&with);
    assert_eq!(cfg.frame_first(), with.aligned(0));
}
