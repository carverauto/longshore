// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Socket slots reused or closed while their old connection is still in
//! flight on [`SimIo`].

use super::{ADDR, LIMITS};
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use std::net::SocketAddr;

/// Submits `ops`, ticks until each has completed; their sorted tokens and results.
fn run(sim: &mut SimIo, out: &mut Completions, ops: &[(u64, Op)]) -> Vec<(u64, i32)> {
    for (t, op) in ops {
        sim.submit(Token(*t), *op).unwrap();
    }
    let mut seen = vec![];
    while seen.len() < ops.len() {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            seen.push((c.token.0, c.result));
        }
    }
    seen.sort();
    seen
}

/// `listener` accepts into `server` (token `tok`) the connect of `client`
/// (token `tok + 1`).
fn accept_connect(listener: Sock, server: Sock, client: Sock, tok: u64) -> [(u64, Op); 2] {
    [
        (
            tok,
            Op::Accept {
                listener,
                sock: server,
            },
        ),
        (
            tok + 1,
            Op::Connect {
                sock: client,
                addr: ADDR,
            },
        ),
    ]
}

/// `client` sends "hello" (token 7) and `server` receives it (token 8).
fn hello(client: Sock, server: Sock) -> [(u64, Op); 2] {
    [
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
                sock: server,
                buf: BufId(1),
                off: 0,
                len: 16,
            },
        ),
    ]
}

/// A closed socket slot can be reused at once. The connection that used to
/// be paired with it must not be able to disturb the new one.
#[test]
fn a_stale_peer_cannot_disturb_a_reused_slot() {
    let mut io = SimIo::new(
        Limits {
            sockets_max: 5,
            ..LIMITS
        },
        11,
        Faults::default(),
    );
    let mut out = Completions::new(8);
    let (listener, client, old_server, new_server) = (Sock(0), Sock(1), Sock(2), Sock(3));
    let _: SocketAddr = io.listen(listener, ADDR).unwrap();
    let old = accept_connect(listener, old_server, client, 1);
    let _: Vec<(u64, i32)> = run(&mut io, &mut out, &old);
    // The client closes and reconnects in the same slot; the server accepts it elsewhere.
    let _: Vec<(u64, i32)> = run(&mut io, &mut out, &[(3, Op::Close { sock: client })]);
    let new = accept_connect(listener, new_server, client, 4);
    let _: Vec<(u64, i32)> = run(&mut io, &mut out, &new);
    // Only now does the old server-side socket close.
    let close_old = [(6, Op::Close { sock: old_server })];
    let _: Vec<(u64, i32)> = run(&mut io, &mut out, &close_old);
    io.buf(BufId(0))[..5].copy_from_slice(b"hello");
    let r = run(&mut io, &mut out, &hello(client, new_server));
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
    let (listener, client, server, later) = (Sock(0), Sock(1), Sock(2), Sock(3));
    let mut hit = 0;
    for seed in 0..200u64 {
        let mut io = SimIo::new(LIMITS, seed, Faults::default());
        let mut out = Completions::new(8);
        let _: SocketAddr = io.listen(listener, ADDR).unwrap();
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
            assert_eq!(
                connect,
                Some(-longshore_common::io::ECANCELED),
                "seed {seed}: {seen:?}"
            );
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
