// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{Completions, Limits};
use longshore_common::sim::scenario::{echo, EchoError, EchoStats};
use longshore_common::sim::{Faults, SimIo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(test)]
#[path = "echo/slots.rs"]
mod slots;

#[global_allocator]
static ALLOC: Guard = Guard;

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(4)
    .with_buffers(3)
    .with_buffer_size(4096)
    .with_ops_max(4);
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
const TOTAL: u64 = 300_000;

/// Runs one echo with allocation forbidden once setup is done.
fn run(seed: u64, faults: Faults) -> (Result<EchoStats, EchoError>, u64) {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut out = Completions::new(8);
    let r = sealed(|| echo(&mut io, ADDR, TOTAL, 4096, false, &mut out));
    (r, io.trace_hash())
}

const BENIGN: Faults = Faults::NONE
    .with_connect_refused(0)
    .with_reset(0)
    .with_short_send(300_000)
    .with_partial_recv(300_000)
    .with_bit_flip(0)
    .with_delay(200_000)
    .with_udp_drop(0);

#[test]
fn echo_survives_short_writes_partial_reads_and_delays() {
    for seed in 0..200 {
        let (r, _) = run(seed, BENIGN);
        assert!(r.is_ok(), "seed {seed}: {r:?}");
    }
}

#[test]
fn same_seed_same_execution() {
    for seed in [1u64, 7, 4242] {
        let a = run(seed, BENIGN);
        let b = run(seed, BENIGN);
        assert_eq!(a, b, "seed {seed} diverged");
    }
    assert_ne!(
        run(1, BENIGN).1,
        run(2, BENIGN).1,
        "different seeds should schedule differently"
    );
}

#[test]
fn failing_seed_reproduces() {
    // Bit flips make the echo fail at a seed-dependent offset. The same seed
    // must fail at the same place with the same trace.
    let faults = BENIGN.with_bit_flip(2_000);
    let (seed, first) = (0..500u64)
        .map(|s| (s, run(s, faults)))
        .find(|(_, r)| matches!(r.0, Err(EchoError::Mismatch(_))))
        .expect("some seed flips a bit");
    for _ in 0..3 {
        assert_eq!(run(seed, faults), first, "seed {seed} did not reproduce");
    }
}

#[test]
fn reset_is_reported_not_hidden() {
    let faults = BENIGN.with_reset(20_000);
    let resets = (0..200u64).filter(|s| matches!(run(*s, faults).0, Err(EchoError::Io(_, r)) if r == -longshore_common::io::ECONNRESET || r == -longshore_common::io::EPIPE)).count();
    assert!(resets > 0, "expected some seeds to hit a reset");
}

/// The echo listens before it connects, so this refusal is the injected
/// fault. A connect with nothing listening is in `sockets.rs`.
#[test]
fn a_refused_connect_is_reported_with_its_token() {
    let mut io = SimIo::new(LIMITS, 1, Faults::NONE.with_connect_refused(1_000_000));
    let mut out = Completions::new(8);
    assert_eq!(
        echo(&mut io, ADDR, 10, 4096, false, &mut out),
        Err(EchoError::Io(2, -longshore_common::io::ECONNREFUSED))
    );
}

#[test]
fn timeouts_fire_in_deadline_order_on_virtual_time() {
    use longshore_common::io::{Io, Op, Token};
    let mut io = SimIo::new(LIMITS, 9, Faults::default());
    let mut out = Completions::new(8);
    for (t, ms) in [(1u64, 30u64), (2, 10), (3, 20)] {
        io.submit(
            Token::new(t),
            Op::Timeout {
                nanos: ms * 1_000_000,
            },
        )
        .unwrap();
    }
    let mut order = vec![];
    while order.len() < 3 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert_eq!(c.result(), 0);
            order.push((c.token().get(), io.now_ns() / 1_000_000));
        }
    }
    assert_eq!(order.iter().map(|o| o.0).collect::<Vec<_>>(), [2, 3, 1]);
    assert!(
        order[0].1 >= 10 && order[2].1 >= 30 && order[2].1 < 40,
        "virtual clock off: {order:?}"
    );
}

#[test]
fn submit_reports_busy_at_the_operation_limit() {
    use longshore_common::io::{Busy, Io, Op, Token};
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    for t in 0..u64::from(LIMITS.ops_max()) {
        io.submit(Token::new(t), Op::Timeout { nanos: 1 }).unwrap();
    }
    assert_eq!(
        io.submit(Token::new(99), Op::Timeout { nanos: 1 }),
        Err(Busy)
    );
}

/// Tick until `log` holds `until` completions, logging each as (token, result).
fn drain(
    sim: &mut SimIo,
    ready: &mut Completions,
    log: &mut Vec<(u64, i32)>,
    until: usize,
) -> std::io::Result<()> {
    use longshore_common::io::Io;
    while log.len() < until {
        sim.tick(true, ready)?;
        while let Some(c) = ready.pop() {
            log.push((c.token().get(), c.result()));
        }
    }
    Ok(())
}

#[test]
fn cancel_stops_a_pending_operation() {
    use longshore_common::io::{BufId, Io, Op, Sock, Token};
    let mut io = SimIo::new(LIMITS, 3, Faults::default());
    let mut out = Completions::new(8);
    let _: SocketAddr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(2),
            addr: ADDR,
        },
    )
    .unwrap();
    let mut seen = vec![];
    drain(&mut io, &mut out, &mut seen, 2).unwrap(); // accept and connect

    // Nothing is ever sent, so this receive would wait forever.
    io.submit(
        Token::new(3),
        Op::Recv {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    for _ in 0..50 {
        io.tick(false, &mut out).unwrap();
    }
    assert!(out.is_empty(), "receive must still be pending");
    io.submit(
        Token::new(9),
        Op::Cancel {
            target: Token::new(3),
        },
    )
    .unwrap();
    drain(&mut io, &mut out, &mut seen, 4).unwrap();
    seen.sort();
    assert_eq!(
        seen,
        [
            (1, 0),
            (2, 0),
            (3, -longshore_common::io::ECANCELED),
            (9, 0)
        ]
    );
    io.submit(
        Token::new(10),
        Op::Cancel {
            target: Token::new(77),
        },
    )
    .unwrap();
    drain(&mut io, &mut out, &mut seen, 5).unwrap();
    assert_eq!(
        seen[4],
        (10, -longshore_common::io::ENOENT),
        "cancelling nothing reports not-found"
    );
}

#[test]
fn files_are_read_whole_and_see_replacement() {
    use longshore_common::io::{BufId, Io, Op, Token};
    let mut io = SimIo::new(LIMITS, 5, Faults::default());
    let mut out = Completions::new(8);
    let f = io.register_file("/etc/credentials").unwrap();
    let mut read = |sim: &mut SimIo| {
        sim.submit(
            Token::new(1),
            Op::ReadFile {
                file: f,
                buf: BufId::new(0),
                off: 0,
                len: 4096,
            },
        )
        .unwrap();
        loop {
            sim.tick(true, &mut out).unwrap();
            if let Some(c) = out.pop() {
                return c.result();
            }
        }
    };
    assert_eq!(
        read(&mut io),
        -longshore_common::io::ENOENT,
        "not created yet"
    );
    io.set_file(f, Some(b"first"));
    assert_eq!(read(&mut io), 5);
    assert_eq!(&io.buf(BufId::new(0))[..5], b"first");
    io.set_file(f, Some(b"second version"));
    assert_eq!(read(&mut io), 14);
    assert_eq!(&io.buf(BufId::new(0))[..14], b"second version");
    io.set_file(f, Some(&[7u8; 5000]));
    assert_eq!(read(&mut io), 4096, "bounded by the length asked for");
}

/// A cancel names its target when it is submitted, as on the real kernel.
/// An operation submitted afterwards under the same token is a different
/// operation and must not be cancelled by it.
#[test]
fn a_cancel_does_not_reach_an_operation_submitted_after_it() {
    use longshore_common::io::{Io, Op, Token, ECANCELED};
    let mut io = SimIo::new(LIMITS, 5, Faults::default());
    let mut out = Completions::new(8);
    // Nothing has token 9 yet, so this cancel has no target.
    io.submit(
        Token::new(1),
        Op::Cancel {
            target: Token::new(9),
        },
    )
    .unwrap();
    io.submit(Token::new(9), Op::Timeout { nanos: 50_000_000 })
        .unwrap();
    let mut results = Vec::new();
    while results.len() < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            results.push((c.token().get(), c.result()));
        }
    }
    results.sort();
    assert_eq!(
        results,
        [(1, -2), (9, 0)],
        "the cancel finds nothing (ENOENT) and the timer runs out normally"
    );
    assert!(io.now_ns() >= 50_000_000);

    // A cancel submitted after its target still cancels it.
    io.submit(Token::new(9), Op::Timeout { nanos: 50_000_000 })
        .unwrap();
    io.submit(
        Token::new(1),
        Op::Cancel {
            target: Token::new(9),
        },
    )
    .unwrap();
    results.clear();
    while results.len() < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            results.push((c.token().get(), c.result()));
        }
    }
    results.sort();
    assert_eq!(results, [(1, 0), (9, -ECANCELED)]);
}
