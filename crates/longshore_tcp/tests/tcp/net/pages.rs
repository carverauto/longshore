// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Receiving in pages (`Rx::ZeroCopy`): what the plan asks and refuses,
//! and what `connect` and `listen` refuse.

use super::paged::{paged, world, world_with, zc};
use super::*;
use longshore_tcp::{Addr, ConfigError, NetError, UnixPath};

fn listens(w: &mut World, addr: impl Into<Addr>, cfg: ListenConfig) -> Result<Listener, NetError> {
    w.server.listen(&mut w.io, addr, cfg)
}

#[test]
fn connect_and_listen_refuse_what_the_mode_cannot_read() {
    let mut w = world(0, Faults::NONE);
    let framing = HeaderFraming::length_prefix(8192).unwrap();
    let bytes = ConnOpts::new(Framing::Bytes).with_rx(Rx::ZeroCopy);
    let at_block = ConnOpts::new(Framing::Header(framing.with_body_at_block(4096).unwrap()))
        .with_rx(Rx::ZeroCopy);
    let unix = Addr::Unix(UnixPath::new("/longshore/zc.sock").unwrap());
    let refused = [
        listens(&mut w, ADDR, ListenConfig::new(bytes)),
        listens(&mut w, ADDR, ListenConfig::new(at_block)),
        listens(&mut w, unix, ListenConfig::new(zc())),
        listens(&mut w, ADDR, ListenConfig::new(zc()).with_reuse_port(true)),
    ];
    for r in refused {
        assert_eq!(r, Err(NetError::Framing));
    }
    let tls = listens(&mut w, ADDR, ListenConfig::new(zc()).with_tls(true));
    assert_eq!(tls, Err(NetError::NoTls));
    assert_eq!(
        w.server.connect(&mut w.io, unix, zc()),
        Err(NetError::Framing)
    );
    // What the mode can read, it may.
    assert!(listens(&mut w, ADDR, ListenConfig::new(zc())).is_ok());
}

#[test]
fn a_set_without_pages_or_without_an_area_refuses_the_mode() {
    // No pages in the sizes: the client's set.
    let mut w = world(0, Faults::NONE);
    assert_eq!(
        w.client.connect(&mut w.io, ADDR, zc()),
        Err(NetError::Framing)
    );
    // Pages in the sizes, and a backend built without an area.
    let paged_world = world_with(0, Faults::NONE, paged(64, 8, 256));
    let limits = paged_world.limits.with_area_pages(0);
    let cfg = *paged_world.server.config();
    let mut bare: SimNet = Net::new(KernelTransport::new(&cfg), cfg, &limits);
    let mut io = SimIo::new(limits, 0, Faults::NONE);
    assert_eq!(
        bare.listen(&mut io, ADDR, ListenConfig::new(zc()))
            .map(|_| ()),
        Err(NetError::Framing)
    );
}

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
