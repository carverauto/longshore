// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) for lookups under the preference for large
//! segments: whatever has been measured, in whatever order, a host with
//! addresses always has one to hand out.

#[cfg(test)]
mod common;
use common::*;
use hegel::generators as gs;
use hegel::TestCase;
use longshore_common::io::Segment;

/// A size a connection might report: nothing, the common ones, or anything.
fn size(tc: &TestCase) -> u32 {
    match tc.draw(gs::integers::<u8>().max_value(4)) {
        0 => 0,
        1 => tc.draw(gs::sampled_from(vec![536, 1432, 4114, 8176, 8228])),
        2 => tc.draw(gs::integers::<u32>().max_value(9000)),
        3 => tc.draw(gs::integers::<u32>().max_value(65_535)),
        _ => tc.draw(gs::integers::<u32>()),
    }
}

/// Any sequence of measurements and changes of preference leaves lookups
/// working: they never come back empty, never hand out an address the host
/// does not have, never pass over one that is unmeasured, never hand out
/// one measured below the floor, and keep as many addresses in use as asked.
#[hegel::test]
fn lookups_always_have_an_address_whatever_was_measured(tc: TestCase) {
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    let mut s = Server::start(&mut io, NS1);
    let mut cfg = config(&[NS1], SEC, Family::V4);
    let mut keep = tc.draw(gs::optional(gs::integers::<usize>().max_value(9)));
    cfg.prefer_large_segments = keep;
    let mut r = Resolver::new(cfg).expect("resolver config");
    r.start(&mut io);
    pump(&mut io, &mut r, &mut s, &mut Completions::new(8), 20 * MS);
    let all = zone_addrs(0);
    assert_eq!(r.addrs(0), &all[..]);
    let stranger = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
    // What the test knows was last measured for each address.
    let mut sizes = [0u32; 3];

    let steps = tc.draw(gs::integers::<usize>().max_value(12));
    for _ in 0..steps {
        match tc.draw(gs::integers::<u8>().max_value(5)) {
            0 => {
                keep = tc.draw(gs::optional(gs::integers::<usize>().max_value(9)));
                r.set_prefer_large_segments(keep);
            }
            1 => {
                let _: u32 = r.connected(
                    0,
                    stranger,
                    Segment {
                        size: size(&tc),
                        path_limit: 0,
                    },
                );
            }
            _ => {
                let k = tc.draw(gs::integers::<usize>().max_value(2));
                let segment = Segment {
                    size: size(&tc),
                    path_limit: if tc.draw(gs::booleans()) {
                        size(&tc)
                    } else {
                        0
                    },
                };
                let floor = r.connected(0, all[k], segment);
                sizes[k] = segment.size;
                assert_eq!(floor, r.segment_floor(0));
            }
        }

        let floor = r.segment_floor(0);
        let largest = sizes.iter().copied().max().unwrap();
        assert!(floor <= largest, "floor {floor} above every size {sizes:?}");
        let mut seen = [false; 3];
        for _ in 0..3 {
            let ip = r.lookup(0).expect("a host with addresses has one to give");
            let k = all
                .iter()
                .position(|a| *a == ip)
                .expect("one of the host's");
            seen[k] = true;
            assert!(
                sizes[k] == 0 || sizes[k] >= floor,
                "handed out {ip} measured {} under the floor {floor}",
                sizes[k]
            );
        }
        let handed = seen.iter().filter(|used| **used).count();
        match keep {
            None => {
                assert_eq!((floor, r.probes_wanted(0)), (0, 0));
                assert_eq!(handed, 3, "no preference: every address in rotation");
            }
            Some(keep) => {
                if floor > 0 {
                    assert!(
                        handed >= keep.clamp(1, 3),
                        "{handed} in use, {keep} asked for, sizes {sizes:?}"
                    );
                    let top = sizes.iter().position(|size| *size == largest).unwrap();
                    assert!(seen[top], "the largest is in use: {sizes:?}");
                    assert_eq!(r.probes_wanted(0), 0);
                }
                for k in 0..3 {
                    if sizes[k] == 0 && floor > 0 {
                        assert!(seen[k], "unmeasured address {k} passed over");
                    }
                }
            }
        }
        // Host 1 was never measured.
        assert_eq!(r.segment_floor(1), 0);
        assert_eq!(r.lookup(1), Some(zone_addrs(1)[0]));
    }
}
