// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Protected connections and frame memory they share.

use super::{certs, sim, sim_addr, NAME};
use longshore_common::io::Limits;
use longshore_common::sim::Faults;
use longshore_tcp::check::{self, secure, World};
use longshore_tcp::{Addr, Conn, ConnOpts, Framing, HeaderFraming, ListenConfig, Listener};

/// Half the runs with receives and sends cut short.
fn faults(seed: u64) -> Faults {
    if seed.is_multiple_of(2) {
        Faults::NONE
    } else {
        Faults::NONE
            .with_partial_recv(300_000)
            .with_short_send(300_000)
    }
}

/// As many protected connections as frame buffers, each receiving frames
/// that span TLS records: how many of `frames` frames each delivered.
fn frames_across_records(seed: u64, conns: u32, buffers: u32, frames: usize) -> Vec<usize> {
    let certs = certs(NAME);
    let framing = HeaderFraming::length_prefix(30_000).unwrap();
    let opts = ConnOpts::new(Framing::Header(framing));
    let mut w = World::new_tls(
        &mut |l| sim(l, seed, faults(seed)),
        Limits::BASE,
        secure::sizes(conns, 0, 0),
        secure::sizes(0, conns, 1).with_frames(buffers, 32 << 10),
        &certs,
    );
    let _: Listener = w.listen(Addr::Ip(sim_addr()), ListenConfig::new(opts));
    let pairs: Vec<(Conn, Conn)> = (0..conns)
        .map(|_| w.connect(Addr::Ip(sim_addr()), opts))
        .collect();
    // Longer than a record, so the second half comes when the first is
    // already in frame memory.
    let frame = |i: usize, n: usize| check::prefixed(20_000 + 100 * i, (i * 16 + n) as u8);
    let mut sent = vec![0usize; pairs.len()];
    let mut got = vec![0usize; pairs.len()];
    for _ in 0..6000 {
        for (i, (c, s)) in pairs.iter().enumerate() {
            if sent[i] < frames && w.send(*c, &frame(i, sent[i])) {
                sent[i] += 1;
            }
            for (f, bytes) in w.pull_all(*s) {
                assert!(
                    bytes == frame(i, got[i]),
                    "connection {i}, frame {}",
                    got[i]
                );
                got[i] += 1;
                w.server().0.release(f);
            }
        }
        w.turn();
        while w.client().0.next_event().is_some() {}
        while w.server().0.next_event().is_some() {}
    }
    got
}

#[test]
fn protected_connections_do_not_wait_on_each_other_for_frame_memory() {
    for seed in 0..20 {
        assert_eq!(
            frames_across_records(seed, 3, 3, 4),
            [4, 4, 4],
            "seed {seed}"
        );
    }
}

#[test]
fn more_protected_connections_than_frame_buffers_all_deliver() {
    for seed in 0..20 {
        let two = frames_across_records(seed, 4, 2, 3);
        assert_eq!(two, [3, 3, 3, 3], "seed {seed}, two buffers");
        let three = frames_across_records(seed, 4, 3, 3);
        assert_eq!(three, [3, 3, 3, 3], "seed {seed}, three buffers");
    }
}
