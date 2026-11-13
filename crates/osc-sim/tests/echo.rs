// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use osc_io::alloc_guard::{sealed, Guard};
use osc_io::{Completions, Limits};
use osc_sim::scenario::{echo, EchoError, EchoStats};
use osc_sim::{Faults, SimIo};
use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

const LIMITS: Limits = Limits {
    sockets_max: 4,
    buffers: 3,
    buffer_size: 4096,
    small_buffers: 0,
    small_buffer_size: 0,
    ops_max: 4,
};
const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 443);
const TOTAL: u64 = 300_000;

/// Runs one echo with allocation forbidden once setup is done.
fn run(seed: u64, faults: Faults) -> (Result<EchoStats, EchoError>, u64) {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut out = Completions::new(8);
    let r = sealed(|| echo(&mut io, ADDR, TOTAL, 4096, false, &mut out));
    (r, io.trace_hash())
}

const BENIGN: Faults = Faults {
    connect_refused: 0,
    reset: 0,
    short_send: 300_000,
    partial_recv: 300_000,
    bit_flip: 0,
    delay: 200_000,
    udp_drop: 0,
};

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
    let faults = Faults {
        bit_flip: 2_000,
        ..BENIGN
    };
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
    let faults = Faults {
        reset: 20_000,
        ..BENIGN
    };
    let resets = (0..200u64).filter(|s| matches!(run(*s, faults).0, Err(EchoError::Io(_, r)) if r == -osc_io::ECONNRESET || r == -osc_io::EPIPE)).count();
    assert!(resets > 0, "expected some seeds to hit a reset");
}

#[test]
fn refused_when_nothing_listens() {
    let mut io = SimIo::new(
        LIMITS,
        1,
        Faults {
            connect_refused: 1_000_000,
            ..Faults::default()
        },
    );
    let mut out = Completions::new(8);
    assert_eq!(
        echo(&mut io, ADDR, 10, 4096, false, &mut out),
        Err(EchoError::Io(2, -osc_io::ECONNREFUSED))
    );
}

#[test]
fn timeouts_fire_in_deadline_order_on_virtual_time() {
    use osc_io::{Io, Op, Token};
    let mut io = SimIo::new(LIMITS, 9, Faults::default());
    let mut out = Completions::new(8);
    for (t, ms) in [(1u64, 30u64), (2, 10), (3, 20)] {
        io.submit(
            Token(t),
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
            assert_eq!(c.result, 0);
            order.push((c.token.0, io.now_ns() / 1_000_000));
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
    use osc_io::{Busy, Io, Op, Token};
    let mut io = SimIo::new(LIMITS, 1, Faults::default());
    for t in 0..LIMITS.ops_max as u64 {
        io.submit(Token(t), Op::Timeout { nanos: 1 }).unwrap();
    }
    assert_eq!(io.submit(Token(99), Op::Timeout { nanos: 1 }), Err(Busy));
}

#[test]
fn cancel_stops_a_pending_operation() {
    use osc_io::{BufId, Io, Op, Sock, Token};
    let mut io = SimIo::new(LIMITS, 3, Faults::default());
    let mut out = Completions::new(8);
    io.listen(Sock(0), ADDR).unwrap();
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
            addr: ADDR,
        },
    )
    .unwrap();
    let mut seen = vec![];
    let drain =
        |io: &mut SimIo, out: &mut Completions, seen: &mut Vec<(u64, i32)>, until: usize| {
            while seen.len() < until {
                io.tick(true, out).unwrap();
                while let Some(c) = out.pop() {
                    seen.push((c.token.0, c.result));
                }
            }
        };
    drain(&mut io, &mut out, &mut seen, 2); // accept and connect
                                            // Nothing is ever sent, so this receive would wait forever.
    io.submit(
        Token(3),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    for _ in 0..50 {
        io.tick(false, &mut out).unwrap();
    }
    assert!(out.is_empty(), "receive must still be pending");
    io.submit(Token(9), Op::Cancel { target: Token(3) })
        .unwrap();
    drain(&mut io, &mut out, &mut seen, 4);
    seen.sort();
    assert_eq!(seen, [(1, 0), (2, 0), (3, -osc_io::ECANCELED), (9, 0)]);
    io.submit(Token(10), Op::Cancel { target: Token(77) })
        .unwrap();
    drain(&mut io, &mut out, &mut seen, 5);
    assert_eq!(
        seen[4],
        (10, -osc_io::ENOENT),
        "cancelling nothing reports not-found"
    );
}

/// A closed socket slot can be reused at once. The connection that used to
/// be paired with it must not be able to disturb the new one.
#[test]
fn a_stale_peer_cannot_disturb_a_reused_slot() {
    use osc_io::{BufId, Io, Op, Sock, Token};
    let mut io = SimIo::new(
        Limits {
            sockets_max: 5,
            ..LIMITS
        },
        11,
        Faults::default(),
    );
    let mut out = Completions::new(8);
    let mut run = |io: &mut SimIo, ops: &[(u64, Op)]| -> Vec<(u64, i32)> {
        for (t, op) in ops {
            io.submit(Token(*t), *op).unwrap();
        }
        let mut seen = vec![];
        while seen.len() < ops.len() {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                seen.push((c.token.0, c.result));
            }
        }
        seen.sort();
        seen
    };
    let (listener, client, old_server, new_server) = (Sock(0), Sock(1), Sock(2), Sock(3));
    io.listen(listener, ADDR).unwrap();
    run(
        &mut io,
        &[
            (
                1,
                Op::Accept {
                    listener,
                    sock: old_server,
                },
            ),
            (
                2,
                Op::Connect {
                    sock: client,
                    addr: ADDR,
                },
            ),
        ],
    );
    // The client closes and reconnects in the same slot; the server accepts it elsewhere.
    run(&mut io, &[(3, Op::Close { sock: client })]);
    run(
        &mut io,
        &[
            (
                4,
                Op::Accept {
                    listener,
                    sock: new_server,
                },
            ),
            (
                5,
                Op::Connect {
                    sock: client,
                    addr: ADDR,
                },
            ),
        ],
    );
    // Only now does the old server-side socket close.
    run(&mut io, &[(6, Op::Close { sock: old_server })]);
    io.buf(BufId(0))[..5].copy_from_slice(b"hello");
    let r = run(
        &mut io,
        &[
            (
                7,
                Op::Send {
                    sock: client,
                    buf: BufId(0),
                    off: 0,
                    len: 5,
                    zero_copy: false,
                },
            ),
            (
                8,
                Op::Recv {
                    sock: new_server,
                    buf: BufId(1),
                    off: 0,
                    len: 16,
                },
            ),
        ],
    );
    assert_eq!(
        r,
        [(7, 5), (8, 5)],
        "the new connection must be unaffected by the old peer closing"
    );
    assert_eq!(&io.buf(BufId(1))[..5], b"hello");
}

/// A connect that the far end has accepted but that has not yet been reported
/// is still in progress. Closing its socket must end it; it must not start
/// over on the now free slot and succeed through a later accept.
#[test]
fn close_ends_an_accepted_connect_not_yet_reported() {
    use osc_io::{Io, Op, Sock, Token};
    let (listener, client, server, later) = (Sock(0), Sock(1), Sock(2), Sock(3));
    let mut hit = 0;
    for seed in 0..200u64 {
        let mut io = SimIo::new(LIMITS, seed, Faults::default());
        let mut out = Completions::new(8);
        io.listen(listener, ADDR).unwrap();
        io.submit(
            Token(1),
            Op::Connect {
                sock: client,
                addr: ADDR,
            },
        )
        .unwrap();
        io.submit(
            Token(2),
            Op::Accept {
                listener,
                sock: server,
            },
        )
        .unwrap();
        let mut seen = vec![];
        while !seen.iter().any(|&(t, _)| t == 2) {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                seen.push((c.token.0, c.result));
            }
        }
        if seen.iter().any(|&(t, _)| t == 1) {
            continue; // the connect was reported in the same pass
        }
        hit += 1;
        io.submit(Token(3), Op::Close { sock: client }).unwrap();
        io.submit(
            Token(4),
            Op::Accept {
                listener,
                sock: later,
            },
        )
        .unwrap();
        for _ in 0..50 {
            io.tick(false, &mut out).unwrap();
            while let Some(c) = out.pop() {
                seen.push((c.token.0, c.result));
            }
        }
        let connect = seen.iter().find(|&&(t, _)| t == 1).map(|&(_, r)| r);
        let close = seen.iter().position(|&(t, _)| t == 3);
        let connect_at = seen.iter().position(|&(t, _)| t == 1);
        if close < connect_at {
            assert_eq!(connect, Some(-osc_io::ECANCELED), "seed {seed}: {seen:?}");
            assert!(
                !seen.iter().any(|&(t, _)| t == 4),
                "seed {seed}: nothing is left to accept: {seen:?}"
            );
        }
    }
    assert!(
        hit > 0,
        "expected some seeds to report the accept before the connect"
    );
}

#[test]
fn files_are_read_whole_and_see_replacement() {
    use osc_io::{BufId, Io, Op, Token};
    let mut io = SimIo::new(LIMITS, 5, Faults::default());
    let mut out = Completions::new(8);
    let f = io.register_file("/etc/credentials").unwrap();
    let mut read = |io: &mut SimIo| {
        io.submit(
            Token(1),
            Op::ReadFile {
                file: f,
                buf: BufId(0),
                off: 0,
                len: 4096,
            },
        )
        .unwrap();
        loop {
            io.tick(true, &mut out).unwrap();
            if let Some(c) = out.pop() {
                return c.result;
            }
        }
    };
    assert_eq!(read(&mut io), -osc_io::ENOENT, "not created yet");
    io.set_file(f, Some(b"first"));
    assert_eq!(read(&mut io), 5);
    assert_eq!(&io.buf(BufId(0))[..5], b"first");
    io.set_file(f, Some(b"second version"));
    assert_eq!(read(&mut io), 14);
    assert_eq!(&io.buf(BufId(0))[..14], b"second version");
    io.set_file(f, Some(&[7u8; 5000]));
    assert_eq!(read(&mut io), 4096, "bounded by the length asked for");
}

/// A cancel names its target when it is submitted, as on the real kernel.
/// An operation submitted afterwards under the same token is a different
/// operation and must not be cancelled by it.
#[test]
fn a_cancel_does_not_reach_an_operation_submitted_after_it() {
    use osc_io::{Io, Op, Token, ECANCELED};
    let mut io = SimIo::new(LIMITS, 5, Faults::default());
    let mut out = Completions::new(8);
    // Nothing has token 9 yet, so this cancel has no target.
    io.submit(Token(1), Op::Cancel { target: Token(9) })
        .unwrap();
    io.submit(Token(9), Op::Timeout { nanos: 50_000_000 })
        .unwrap();
    let mut results = Vec::new();
    while results.len() < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            results.push((c.token.0, c.result));
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
    io.submit(Token(9), Op::Timeout { nanos: 50_000_000 })
        .unwrap();
    io.submit(Token(1), Op::Cancel { target: Token(9) })
        .unwrap();
    let mut results = Vec::new();
    while results.len() < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            results.push((c.token.0, c.result));
        }
    }
    results.sort();
    assert_eq!(results, [(1, 0), (9, -ECANCELED)]);
}
