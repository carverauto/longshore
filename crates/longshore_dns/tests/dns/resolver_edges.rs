// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The resolver where the simulator's usual order of events does not take
//! it: an answer that overtakes the completion of the send that asked for
//! it, nameservers that are all dead or cannot be reached at all, tokens at
//! the ends of its range, and configurations it refuses.

#[cfg(test)]
mod common;
use common::*;
use longshore_common::io::Completion;
use longshore_dns::ConfigError;

const BASE: u64 = 1 << 40;
const K_CONNECT: u64 = BASE;
const K_RECV: u64 = BASE + 8;
const K_SEND: u64 = BASE + 16;
const NOWHERE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99)), 53);

fn completion(token: u64, result: i32) -> Completion {
    Completion::new(Token(token), result)
}

/// On a real kernel an answer can be reaped before the completion of the
/// send that asked for it. The next query then has to wait for the transmit
/// buffer, and must go out as soon as the send completes, not at a timeout.
#[test]
fn a_query_waits_for_the_previous_send_to_complete_then_goes_out() {
    let (mut io, mut r, mut s, mut out) = world(1, Faults::default(), &[NS1], NS1, SEC);
    let mut held: Option<Completion> = None;
    let mut overtaken = 0;
    while io.now_ns() < 40 * MS {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == K_SEND {
                // Keep the send's completion back until its answer is in.
                assert!(held.is_none(), "one send in flight at a time");
                held = Some(c);
                continue;
            }
            let answer = c.token.0 == K_RECV;
            if !r.on_completion(&mut io, c) {
                s.on(&mut io, c.token.0, c.result);
            }
            if answer {
                if let Some(send) = held.take() {
                    overtaken += 1;
                    assert!(r.on_completion(&mut io, send));
                }
            }
        }
    }
    assert_eq!(overtaken, 2, "both answers overtook their sends");
    assert!(r.ready(), "resolved well inside one answer timeout");
    assert_eq!(
        (r.addrs(0), r.addrs(1)),
        (&zone_addrs(0)[..], &zone_addrs(1)[..])
    );
    assert_eq!(
        (s.queries, r.queries(), r.cycles(), r.failures()),
        (2, 2, 1, 0)
    );
}

/// With every nameserver dead, each question is put to each server once,
/// and the cycle still ends.
#[test]
fn with_both_nameservers_dead_each_is_tried_once_per_host() {
    // The only server there is listens somewhere the resolver does not ask.
    let (mut io, mut r, mut s, mut out) = world(4, Faults::default(), &[NS1, NS2], NOWHERE, SEC);
    pump(&mut io, &mut r, &mut s, &mut out, 400 * MS);
    assert!(!r.ready() && r.lookup(0).is_none() && r.lookup(1).is_none());
    assert_eq!(
        (r.queries(), r.failures(), r.cycles(), s.queries),
        (4, 4, 1, 0),
        "two hosts, two servers, one timeout each"
    );
    // And the next cycle starts a refresh interval later, not sooner.
    pump(&mut io, &mut r, &mut s, &mut out, SEC);
    assert_eq!((r.queries(), r.cycles()), (4, 1));
}

/// A nameserver whose socket could not be opened is not received from: its
/// queries time out, one failure each, and nothing spins on the dead socket.
#[test]
fn a_nameserver_that_cannot_be_connected_costs_timeouts_and_no_more() {
    let mut r = Resolver::new(config(&[NS1], SEC, Family::V4)).expect("resolver config");
    r.start(&mut SimIo::new(LIMITS, 1, Faults::default()));
    // A world in which that connect did not happen: the resolver is told
    // it failed, and its socket slot is not a socket.
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    assert!(r.on_completion(&mut io, completion(K_CONNECT, -111)));
    assert_eq!((r.failures(), r.queries()), (1, 1), "it asks all the same");
    let mut out = Completions::new(8);
    while io.now_ns() < 200 * MS {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert!(r.on_completion(&mut io, c));
        }
    }
    // The connect, then per host a send that fails and a query that times out.
    assert_eq!((r.failures(), r.queries(), r.cycles()), (5, 2, 1));
    assert!(!r.ready());
}

#[test]
fn only_tokens_in_the_resolvers_range_are_its_own() {
    let mut r = Resolver::new(config(&[NS1], SEC, Family::V4)).expect("resolver config");
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    for foreign in [0, 1, BASE - 1, BASE + 256, BASE + 257, u64::MAX] {
        assert!(
            !r.on_completion(&mut io, completion(foreign, 0)),
            "{foreign}"
        );
    }
    // The top of its range is its own, and means nothing.
    for unused in [BASE + 18, BASE + 255] {
        assert!(r.on_completion(&mut io, completion(unused, 0)), "{unused}");
        assert!(r.on_completion(&mut io, completion(unused, -5)), "{unused}");
    }
    assert_eq!((r.failures(), r.queries(), r.cycles()), (0, 0, 0));
    let mut out = Completions::new(8);
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty(), "and it submitted nothing");
}

/// Tokens in the resolver's range that name a server it does not have: the
/// range leaves room for eight, the resolver has at most two. A stray
/// completion there is ignored like the unused top of the range, with any
/// result, before and after the resolver has started.
#[test]
fn a_completion_for_a_server_it_does_not_have_is_ignored() {
    let strays = || (2..8).chain(10..16).map(|k| BASE + k);
    {
        let mut r = Resolver::new(config(&[NS1], SEC, Family::V4)).expect("resolver config");
        let mut io = SimIo::new(LIMITS, 1, Faults::default());
        for token in strays() {
            for result in [0, 12, 512, -5] {
                assert!(
                    r.on_completion(&mut io, completion(token, result)),
                    "{token}"
                );
            }
        }
        assert_eq!((r.failures(), r.queries(), r.cycles()), (0, 0, 0));
        let mut out = Completions::new(8);
        io.tick(false, &mut out).unwrap();
        assert!(out.is_empty(), "and it submitted nothing");
    }

    // The same while it is working: resolution is not disturbed.
    let (mut io, mut r, mut s, mut out) = world(1, Faults::default(), &[NS1], NS1, SEC);
    for token in strays() {
        assert!(r.on_completion(&mut io, completion(token, 40)), "{token}");
    }
    while io.now_ns() < 40 * MS {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if !r.on_completion(&mut io, c) {
                s.on(&mut io, c.token.0, c.result);
            }
        }
    }
    assert!(r.ready());
    assert_eq!(r.failures(), 0);
}

#[test]
fn a_host_is_found_by_name_whatever_its_case_or_trailing_dot() {
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.hosts = vec!["S3.Example.com.".into(), "blob.example.net".into()];
    let r = Resolver::new(cfg).expect("resolver config");
    assert_eq!(r.host_index("s3.example.com"), Some(0));
    assert_eq!(r.host_index("s3.EXAMPLE.com."), Some(0));
    assert_eq!(r.host_index("BLOB.example.net."), Some(1));
    for absent in [
        "",
        ".",
        "example.com",
        "s3.example.com..",
        "xs3.example.com",
    ] {
        assert_eq!(r.host_index(absent), None, "{absent:?}");
    }
}

/// Buffers too small for the resolver are refused when it starts, by name,
/// and not at the first query or answer that happens not to fit.
#[test]
#[should_panic(expected = "rx_bufs")]
fn a_receive_buffer_smaller_than_a_message_is_refused_at_start() {
    let limits = Limits {
        buffer_size: 511,
        ..LIMITS
    };
    let mut io = SimIo::new(limits, 1, Faults::default());
    Resolver::new(config(&[NS1], SEC, Family::V4))
        .expect("resolver config")
        .start(&mut io);
}

#[test]
#[should_panic(expected = "tx_buf")]
fn a_send_buffer_smaller_than_the_longest_query_is_refused_at_start() {
    // Room for a whole answer in the large buffers, and a small one of 20
    // bytes to send from: less than any query for these hosts.
    let limits = Limits {
        small_buffers: 1,
        small_buffer_size: 20,
        ..LIMITS
    };
    let mut io = SimIo::new(limits, 1, Faults::default());
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.tx_buf = limits.small(0);
    Resolver::new(cfg).expect("resolver config").start(&mut io);
}

#[test]
fn no_nameserver_is_refused() {
    let refused = Resolver::new(config(&[], SEC, Family::V4)).err();
    assert_eq!(refused, Some(ConfigError::Servers(0)));
}

#[test]
fn three_nameservers_are_refused() {
    let refused = Resolver::new(config(&[NS1, NS2, NOWHERE], SEC, Family::V4)).err();
    assert_eq!(refused, Some(ConfigError::Servers(3)));
}

#[test]
fn no_hostname_is_refused() {
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.hosts.clear();
    assert_eq!(Resolver::new(cfg).err(), Some(ConfigError::NoHosts));
}

#[test]
fn a_hostname_that_is_not_one_is_refused() {
    let mut cfg = config(&[NS1], SEC, Family::V6);
    cfg.hosts.push("not a host".into());
    let refused = Resolver::new(cfg).err();
    assert_eq!(refused, Some(ConfigError::Hostname("not a host".into())));
    assert_eq!(
        refused.map(|e| e.to_string()),
        Some("invalid hostname \"not a host\"".into())
    );
}

#[test]
fn a_refresh_interval_of_zero_is_refused() {
    let refused = Resolver::new(config(&[NS1], 0, Family::V4)).err();
    assert_eq!(refused, Some(ConfigError::ZeroInterval));
}

#[test]
fn an_answer_timeout_of_zero_is_refused() {
    let mut cfg = config(&[NS1], SEC, Family::V4);
    cfg.timeout_ns = 0;
    assert_eq!(Resolver::new(cfg).err(), Some(ConfigError::ZeroInterval));
}
