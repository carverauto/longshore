// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The spec's scenario "Sharing the ring": a `Files` set and two
//! `longshore_tcp` sets planned onto one `Limits`, on one simulator,
//! driven by one loop; frames go over a connection and writes to a file in
//! the same turns, and each set takes only its own completions.

use super::sim::sim;
use longshore_common::io::extents::Extents;
use longshore_common::io::{Completion, Io, Limits, Token};
use longshore_common::sim::SimIo;
use longshore_file::check::{bytes, rw_create, BLOCK};
use longshore_file::{Event, FileConfig, FileSizes, Files, Ticket};
use longshore_tcp::check::{prefix, prefixed, sizes, World, STORAGE};
use longshore_tcp::{Addr, Conn};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const ROUNDS: usize = 12;

/// The world of two connection sets with a file set planned in, its
/// extents, and a connection up.
fn world() -> (World<SimIo>, Files, Extents, (Conn, Conn)) {
    // The file set's tokens are the world's storage range, which the world
    // leaves to its owner; its extents are aligned buffers before the
    // connections' frames. Both share the small class, of the connections'
    // segment size.
    let base = Limits::BASE
        .with_small_buffer_size(4096)
        .with_aligned_buffers(4)
        .with_aligned_buffer_size(8192)
        .with_block_size(BLOCK);
    let fsizes = FileSizes::new(1, 2)
        .with_shared(1)
        .with_paths(1)
        .with_sequence_max(2)
        .with_spans_max(2)
        .with_token_base(STORAGE);
    let (planned, config) = FileConfig::plan(base, fsizes).unwrap();
    assert!(config.tokens() <= 64, "within the world's storage tokens");
    let mut built = None;
    let mut w = World::new(
        &mut |l| {
            built = Some(l);
            sim(l)
        },
        planned,
        sizes(1, 0, 0),
        sizes(0, 1, 1),
    );
    let limits = built.unwrap();
    let files = Files::new(config, &limits);
    let x = Extents::new(limits.aligned(0), base.aligned_buffers());
    let addr = Addr::Ip(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9)),
        7100,
    ));
    let (c, s) = w.pair(addr, prefix());
    (w, files, x, (c, s))
}

#[test]
fn a_file_set_and_connections_share_one_ring() {
    let (mut w, mut files, mut x, (c, s)) = world();
    let open = files.open(w.io(), "/shared.dat", rw_create()).unwrap();
    let mut file = None;
    let (mut frames, mut wrote, mut sent) = (Vec::new(), 0, 0);
    let mut pending: Option<Ticket> = Some(open);
    for _ in 0..20_000 {
        if frames.len() == ROUNDS && wrote == ROUNDS && pending.is_none() {
            break;
        }
        if sent < ROUNDS && w.send(c, &prefixed(300 + sent, sent as u8)) {
            sent += 1;
        }
        if let (Some(f), None, true) = (file, pending, wrote < ROUNDS) {
            let e = x.take().unwrap();
            let data = bytes(wrote as u8, BLOCK as usize);
            w.io().buf(e.buf())[..data.len()].copy_from_slice(&data);
            let off = (wrote as u64) * u64::from(BLOCK);
            pending = Some(
                files
                    .write_at(w.io(), &mut x, f, (e, 0, BLOCK), off)
                    .unwrap(),
            );
            let _freed: bool = x.release(e).unwrap();
        }
        w.turn();
        while let Some(done) = w.next_storage() {
            assert!(
                files.on_completion(w.io(), &mut x, done),
                "{done:?} is the file set's"
            );
        }
        files.poll(w.io(), &mut x);
        while let Some(Event::Done {
            ticket,
            file: f,
            result,
        }) = files.next_event()
        {
            assert_eq!(Some(ticket), pending, "an event of the set's own");
            if ticket == open {
                file = f;
            } else {
                assert_eq!(result, Ok(u64::from(BLOCK)));
                wrote += 1;
            }
            pending = None;
        }
        while w.client().0.next_event().is_some() {}
        while w.server().0.next_event().is_some() {}
        for (frame, got) in w.pull_all(s) {
            frames.push(got);
            w.server().0.release(frame);
        }
    }
    let want: Vec<Vec<u8>> = (0..ROUNDS).map(|i| prefixed(300 + i, i as u8)).collect();
    assert_eq!(frames, want, "every frame, in order");
    assert_eq!(wrote, ROUNDS, "every write");
    // A completion of another set's operation is not the file set's.
    let other = Completion::new(Token::new(STORAGE + 64), 0);
    assert!(!files.on_completion(w.io(), &mut x, other));
    assert_eq!(files.usage().ops(), 0);
}
