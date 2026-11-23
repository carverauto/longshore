// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Responses that are not the answer to the outstanding query. The
//! simulator loses a corrupted datagram, as a UDP checksum would, so the
//! lossy-network tests never show the resolver one. Here the nameserver
//! sends them on purpose.

#[cfg(test)]
mod common;
use common::*;

/// Responses the resolver must take for somebody else's.
const NOT_OURS: [Forge; 4] = [
    Forge::WrongId,
    Forge::WrongQuestion,
    Forge::NotAResponse,
    Forge::CutShort,
];

fn forging(forge: Forge, then_true: bool) -> (SimIo, Resolver, Server, Completions) {
    let (io, r, mut s, out) = world(1, Faults::default(), &[NS1], NS1, SEC);
    s.set_forge(forge, then_true);
    (io, r, s, out)
}

/// A response that is not ours neither answers the query nor ends the wait
/// for its answer: the resolver caches nothing, and goes on waiting until
/// the query's own time runs out.
#[test]
fn a_response_that_is_not_the_answer_is_ignored_and_the_wait_goes_on() {
    for forge in NOT_OURS {
        let (mut io, mut r, mut s, mut out) = forging(forge, false);
        // The answer timeout is 50 ms, and the forged response came within
        // one. The pump stops at the first event past its limit, and the
        // resolver's timer makes one every 12.5 ms: this stops near 37 ms.
        pump(&mut io, &mut r, &mut s, &mut out, 30 * MS);
        assert!(io.now_ns() < 45 * MS, "stopped before the answer timeout");
        assert_eq!(
            (s.queries(), r.queries(), r.cycles(), r.failures()),
            (1, 1, 0, 0),
            "{forge:?}: still waiting for the first answer"
        );
        assert!(!r.ready() && r.lookup(0).is_none(), "{forge:?}");
        assert!(r.addrs(0).is_empty() && r.addrs(1).is_empty(), "{forge:?}");

        // Each host's query then times out, once.
        pump(&mut io, &mut r, &mut s, &mut out, 300 * MS);
        assert_eq!(
            (s.queries(), r.queries(), r.cycles(), r.failures()),
            (2, 2, 1, 2),
            "{forge:?}: one timeout per host"
        );
        assert!(!r.ready() && r.lookup(0).is_none(), "{forge:?}");
        assert!(r.addrs(0).is_empty() && r.addrs(1).is_empty(), "{forge:?}");
    }
}

/// The true answer, arriving after one that is not ours, is still taken:
/// the forged one cost nothing.
#[test]
fn the_true_answer_after_a_forged_one_is_taken() {
    for forge in NOT_OURS {
        let (mut io, mut r, mut s, mut out) = forging(forge, true);
        pump(&mut io, &mut r, &mut s, &mut out, 40 * MS);
        assert!(r.ready(), "{forge:?}");
        assert_eq!(
            (r.addrs(0), r.addrs(1)),
            (&zone_addrs(0)[..], &zone_addrs(1)[..]),
            "{forge:?}"
        );
        assert_eq!(
            (s.queries(), r.queries(), r.cycles(), r.failures()),
            (2, 2, 1, 0),
            "{forge:?}: no retry and no failure"
        );
    }
}

/// An answer the server marks as truncated is an answer: the resolver does
/// not wait for another, counts a failure, and has no addresses from it.
#[test]
fn a_first_answer_marked_truncated_is_a_failure_at_once() {
    let (mut io, mut r, mut s, mut out) = forging(Forge::TcBit, false);
    pump(&mut io, &mut r, &mut s, &mut out, 40 * MS);
    assert_eq!(
        (s.queries(), r.queries(), r.cycles(), r.failures()),
        (2, 2, 1, 2)
    );
    assert!(!r.ready() && r.addrs(0).is_empty());
}

#[test]
fn an_answer_marked_truncated_keeps_the_previous_addresses() {
    let (mut io, mut r, mut s, mut out) = forging(Forge::Nothing, false);
    pump(&mut io, &mut r, &mut s, &mut out, 100 * MS);
    assert!(r.ready());
    assert_eq!((r.cycles(), r.failures()), (1, 0));
    s.set_forge(Forge::TcBit, false);
    // The refresh interval is a second: one more cycle by 1.5 s.
    pump(&mut io, &mut r, &mut s, &mut out, 3 * SEC / 2);
    assert_eq!(
        (r.cycles(), r.failures()),
        (2, 2),
        "one failure per host in the truncated cycle"
    );
    assert_eq!(
        (r.addrs(0), r.addrs(1)),
        (&zone_addrs(0)[..], &zone_addrs(1)[..])
    );
    assert!(r.ready() && r.lookup(0).is_some());
    // And a good answer afterwards is taken as usual.
    s.set_forge(Forge::Nothing, false);
    pump(&mut io, &mut r, &mut s, &mut out, 5 * SEC / 2);
    assert_eq!((r.cycles(), r.failures()), (3, 2));
}
