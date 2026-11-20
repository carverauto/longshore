// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 3: `Virtual` world & `SimIo` trace parity (`#41`).

use longshore::io::{BufId, Completions, Io, Limits, Op, Segment, Sock, Token, EAGAIN, ECANCELED};
use longshore::sim::scenario::echo;
use longshore::sim::{Faults, SimIo};
use std::collections::BTreeSet;
use std::net::SocketAddr;

pub(super) fn sim_limits() -> Limits {
    Limits {
        sockets_max: 6,
        buffers: 4,
        buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    }
}

pub(super) fn drain_exact(
    io: &mut SimIo,
    out: &mut Completions,
    expected: usize,
) -> Vec<(u64, i32)> {
    let mut got = Vec::with_capacity(expected);
    while got.len() < expected {
        io.tick(true, out).expect("tick");
        while let Some(c) = out.pop() {
            got.push((c.token.0, c.result));
        }
    }
    got
}

pub(super) fn run_all_ops_trace(seed: u64, faults: Faults) -> (Vec<(u64, i32)>, u64) {
    let tcp_addr: SocketAddr = "192.0.2.1:443".parse().unwrap();
    let udp_addr: SocketAddr = "192.0.2.2:5353".parse().unwrap();
    let (listener, server, client, udp_srv, udp_cli) =
        (Sock(0), Sock(1), Sock(2), Sock(3), Sock(4));

    let mut io = SimIo::new(sim_limits(), seed, faults);
    let mut out = Completions::new(8);
    let mut log = Vec::new();

    let file = io.register_file("creds.txt").unwrap();
    io.set_file(file, Some(b"secret-key"));
    io.listen(listener, tcp_addr).unwrap();
    io.bind_udp(udp_srv, udp_addr);
    let waker = io.waker().unwrap();
    waker.wake();

    io.submit(Token(1), Op::AwaitWake).unwrap();
    io.submit(
        Token(2),
        Op::ReadFile {
            file,
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(Token(3), Op::Timeout { nanos: 400_000 }).unwrap();
    io.submit(Token(4), Op::Timeout { nanos: 5_000_000 })
        .unwrap();
    io.submit(Token(5), Op::Cancel { target: Token(4) })
        .unwrap();
    io.submit(
        Token(6),
        Op::ConnectUdp {
            sock: udp_cli,
            addr: udp_addr,
        },
    )
    .unwrap();
    log.extend(drain_exact(&mut io, &mut out, 6));

    io.buf(BufId(1))[..4].copy_from_slice(b"ping");
    io.submit(
        Token(7),
        Op::Send {
            sock: udp_cli,
            buf: BufId(1),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(
        Token(8),
        Op::Recv {
            sock: udp_srv,
            buf: BufId(2),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(
        Token(9),
        Op::Accept {
            listener,
            sock: server,
        },
    )
    .unwrap();
    io.submit(
        Token(10),
        Op::Connect {
            sock: client,
            addr: tcp_addr,
        },
    )
    .unwrap();
    log.extend(drain_exact(&mut io, &mut out, 4));

    io.submit(
        Token(11),
        Op::RecvNow {
            sock: server,
            buf: BufId(3),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(Token(12), Op::Cancel { target: Token(999) })
        .unwrap();
    log.extend(drain_exact(&mut io, &mut out, 2));

    let payload = b"hello-stream-payload";
    let (mut sent, mut recvd, mut tok) = (0usize, 0usize, 20u64);
    io.buf(BufId(0))[..payload.len()].copy_from_slice(payload);
    while recvd < payload.len() {
        let mut submitted = 0;
        if sent < payload.len() {
            io.submit(
                Token(tok),
                Op::Send {
                    sock: client,
                    buf: BufId(0),
                    off: sent as u32,
                    len: (payload.len() - sent) as u32,
                    zero_copy: true,
                },
            )
            .unwrap();
            tok += 1;
            submitted += 1;
        }
        let rtok = tok;
        io.submit(
            Token(rtok),
            Op::Recv {
                sock: server,
                buf: BufId(3),
                off: recvd as u32,
                len: (payload.len() - recvd) as u32,
            },
        )
        .unwrap();
        tok += 1;
        submitted += 1;
        for (t, r) in drain_exact(&mut io, &mut out, submitted) {
            log.push((t, r));
            if t == rtok {
                recvd += r as usize;
            } else {
                sent += r as usize;
            }
        }
    }

    for (idx, sock) in [client, server, listener, udp_cli, udp_srv]
        .into_iter()
        .enumerate()
    {
        io.submit(Token(100 + idx as u64), Op::Close { sock })
            .unwrap();
    }
    log.extend(drain_exact(&mut io, &mut out, 5));
    (log, io.trace_hash())
}

#[test]
fn t1_f03_1_echo_and_golden_trace_hashes_without_faults() {
    let addr: SocketAddr = "192.0.2.1:443".parse().unwrap();
    for seed in 0..10u64 {
        let (mut io_a, mut io_b) = (
            SimIo::new(sim_limits(), seed, Faults::default()),
            SimIo::new(sim_limits(), seed, Faults::default()),
        );
        let (mut out_a, mut out_b) = (Completions::new(8), Completions::new(8));
        let sa = echo(&mut io_a, addr, 64 * 1024, 4096, false, &mut out_a).expect("echo a");
        let sb = echo(&mut io_b, addr, 64 * 1024, 4096, false, &mut out_b).expect("echo b");
        assert_eq!(sa, sb);
        assert_eq!(io_a.trace_hash(), io_b.trace_hash(), "seed {seed}");
    }

    // Golden pre-split trace hashes across all 11 Op variants without delay.
    let golden = [
        (1u64, 7_838_426_690_417_349_226u64),
        (7, 189_675_162_845_082_480),
        (42, 12_861_308_534_940_729_908),
        (99, 15_422_325_490_640_172_234),
    ];
    for (seed, want) in golden {
        let (_, hash) = run_all_ops_trace(seed, Faults::default());
        assert_eq!(hash, want, "golden hash mismatch for seed {seed}");
    }
}

#[test]
fn t1_f03_2_echo_and_golden_trace_hashes_with_delay() {
    let addr: SocketAddr = "192.0.2.1:443".parse().unwrap();
    let faults = Faults {
        delay: 200_000,
        ..Default::default()
    };
    for seed in 0..10u64 {
        let mut io_plain = SimIo::new(sim_limits(), seed, Faults::default());
        let mut io_a = SimIo::new(sim_limits(), seed, faults);
        let mut io_b = SimIo::new(sim_limits(), seed, faults);
        let (mut o0, mut oa, mut ob) = (
            Completions::new(8),
            Completions::new(8),
            Completions::new(8),
        );
        let sp = echo(&mut io_plain, addr, 64 * 1024, 4096, false, &mut o0).unwrap();
        let sa = echo(&mut io_a, addr, 64 * 1024, 4096, false, &mut oa).unwrap();
        let sb = echo(&mut io_b, addr, 64 * 1024, 4096, false, &mut ob).unwrap();
        assert_eq!(sa, sb);
        assert_eq!(io_a.trace_hash(), io_b.trace_hash());
        assert!(sa.ticks > sp.ticks, "seed {seed} should show delay ticks");
    }

    let golden = [
        (1u64, 9_502_771_201_062_475_408u64),
        (7, 14_816_035_989_744_244_144),
        (42, 18_060_134_760_023_850_936),
        (99, 8_169_797_375_893_241_630),
    ];
    for (seed, want) in golden {
        let (_, hash) = run_all_ops_trace(seed, faults);
        assert_eq!(hash, want, "delay golden mismatch for seed {seed}");
    }
}

#[test]
fn t1_f03_3_composite_all_ops_deterministic_replay() {
    let faults = Faults {
        delay: 100_000,
        ..Default::default()
    };
    let (log1, hash1) = run_all_ops_trace(31415, faults);
    let (log2, hash2) = run_all_ops_trace(31415, faults);
    assert_eq!(log1, log2);
    assert_eq!(hash1, hash2);
    assert!(log1.iter().any(|&(t, r)| t == 11 && r == -EAGAIN));
    assert!(log1.iter().any(|&(t, r)| t == 4 && r == -ECANCELED));
}

#[test]
fn t1_f03_4_distinct_seeds_produce_distinct_trace_hashes() {
    let addr: SocketAddr = "192.0.2.1:443".parse().unwrap();
    let faults = Faults {
        delay: 200_000,
        short_send: 100_000,
        ..Default::default()
    };
    let mut hashes = BTreeSet::new();
    for seed in 1..=6u64 {
        let mut io = SimIo::new(sim_limits(), seed, faults);
        let mut out = Completions::new(8);
        echo(&mut io, addr, 64 * 1024, 4096, false, &mut out).expect("echo");
        hashes.insert(io.trace_hash());
    }
    assert_eq!(hashes.len(), 6);
}

#[test]
fn t1_f03_5_routed_segment_measurement_preserves_trace() {
    let listener_addr: SocketAddr = "192.0.2.1:443".parse().unwrap();
    let routed_addr: SocketAddr = "192.0.2.99:443".parse().unwrap();
    let run_once = || {
        let mut io = SimIo::new(sim_limits(), 77, Faults::default());
        let mut out = Completions::new(8);
        io.listen(Sock(0), listener_addr).unwrap();
        io.route(routed_addr, listener_addr, 8228);
        io.set_path_limit(8960);
        io.measure_segment_sizes(true);
        io.submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(1),
            },
        )
        .unwrap();
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(2),
                addr: routed_addr,
            },
        )
        .unwrap();
        let mut cs = drain_exact(&mut io, &mut out, 2);
        cs.sort_by_key(|x| x.0);
        assert_eq!(cs[0], (1, 0));
        assert_eq!(
            Segment::decode(cs[1].1),
            Some(Segment {
                size: 8228,
                path_limit: 8960
            })
        );
        assert_eq!(io.route_connects(routed_addr), 1);
        io.trace_hash()
    };
    assert_eq!(run_once(), run_once());
}
