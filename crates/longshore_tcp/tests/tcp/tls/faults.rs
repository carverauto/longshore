// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What ends a protected stream badly, in both receive modes, with frame
//! memory there and without: a close alert, a stream cut without one, and
//! a record with a flipped bit. A frame received whole before the end is
//! delivered; a record that does not authenticate never is.

use super::ending::{turn_and_hear, Heard};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Moment {
    /// The peer shuts its side down: a close alert, then the end.
    Alert,
    /// The peer closes: the end of the stream with no close alert.
    Cut,
    /// A bit of the frame's record is flipped on its way.
    Flip,
}

/// A frame is on its way to a connection that is not reading when
/// `moment` happens; with `starved`, all frame memory is in use elsewhere
/// when it reads again. What its caller pulls after that, and hears.
fn after(seed: u64, rx: Rx, moment: Moment, starved: bool) -> Heard {
    let certs = certs(NAME);
    let mut w = World::new_tls(
        &mut |l| sim(l, seed, Faults::NONE),
        Limits::BASE,
        secure::sizes(2, 0, 0),
        secure::sizes(0, 1, 1),
        &certs,
    );
    // One held frame stops the connection reading.
    let opts = prefix().with_rx(rx).with_held_max(1);
    let (c, s) = w.pair(Addr::Ip(sim_addr()), opts);
    assert!(w.send(c, &check::prefixed(10, 1)));
    let mut held = Vec::new();
    while held.is_empty() {
        w.turn();
        if w.server().0.next_event().is_some() {
            held = w.pull_all(s);
        }
    }
    if moment == Moment::Flip {
        w.io().set_faults(Faults::NONE.with_bit_flip(1_000_000));
    }
    assert!(w.send(c, &check::prefixed(100, 2)));
    let mut heard = Heard {
        got: Vec::new(),
        ended: None,
    };
    for _ in 0..100 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    let (client, io) = w.client();
    match moment {
        Moment::Alert => client.shutdown_write(io, c).unwrap(),
        Moment::Cut => client.close(io, c).unwrap(),
        Moment::Flip => {}
    }
    for _ in 0..100 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    // It reads again.
    let (server, _) = w.server();
    for (f, _) in held.drain(..) {
        server.release(f);
    }
    let elsewhere: Vec<_> = if starved {
        std::iter::from_fn(|| server.extents().take()).collect()
    } else {
        Vec::new()
    };
    for _ in 0..100 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    for ext in elsewhere {
        assert_eq!(w.server().0.extents().release(ext), Ok(true));
    }
    for _ in 0..400 {
        turn_and_hear(&mut w, s, &mut heard);
    }
    heard
}

#[test]
fn a_bad_end_delivers_what_came_whole_and_nothing_that_did_not_authenticate() {
    let whole = [check::prefixed(100, 2)];
    for seed in 0..20 {
        for rx in [Rx::Ring, Rx::Direct] {
            for starved in [false, true] {
                let at = format!("seed {seed}, {rx:?}, starved: {starved}");
                let alert = after(seed, rx, Moment::Alert, starved);
                assert!(
                    alert.got == whole,
                    "{at}: alert, {} frames",
                    alert.got.len()
                );
                assert_eq!(alert.ended, Some(Why::Orderly), "{at}: alert");
                let cut = after(seed, rx, Moment::Cut, starved);
                assert!(cut.got == whole, "{at}: cut, {} frames", cut.got.len());
                assert_eq!(cut.ended, Some(Why::Reset), "{at}: cut");
                let flip = after(seed, rx, Moment::Flip, starved);
                assert!(flip.got.is_empty(), "{at}: a flipped record was delivered");
                assert_eq!(flip.ended, Some(Why::Tls), "{at}: flip");
            }
        }
    }
}
