// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 6: `Posix` world (`kqueue` & `epoll`) (`#41`).

#[cfg(unix)]
use super::f06_posix_world::connect_pair;
#[cfg(all(unix, feature = "fault-injection"))]
use crate::e2e::harness::drive_until;
#[cfg(unix)]
use crate::e2e::harness::{
    drain_completions, posix_backend, submit_and_wait_one, write_pool_bytes,
};
#[cfg(unix)]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token, EAGAIN, ECONNRESET, EPIPE};

#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn t2_f06_1_posix_stale_readiness_among_real_ready_ops() {
    let limits = Limits {
        sockets_max: 6,
        buffers: 3,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    connect_pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));
    connect_pair(&mut io, &mut out, Sock(3), Sock(4), Sock(5));

    write_pool_bytes(&mut io, BufId(0), 0, b"real");
    let _: longshore::io::Completion = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(10),
        Op::Send {
            sock: Sock(2),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
        100,
    )
    .unwrap();

    io.submit(
        Token(11),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.submit(
        Token(12),
        Op::Recv {
            sock: Sock(4),
            buf: BufId(2),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.inject_stale(Sock(4), false);

    let cs = drive_until(&mut io, &mut out, 10, |v| !v.is_empty()).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!((cs[0].token, cs[0].result), (Token(11), 4));

    // Sock(4) remains pending and receives real data when sent.
    write_pool_bytes(&mut io, BufId(0), 0, b"late");
    let _: longshore::io::Completion = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(13),
        Op::Send {
            sock: Sock(5),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
        100,
    )
    .unwrap();
    let late = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
    assert_eq!((late[0].token, late[0].result), (Token(12), 4));
}

#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn t2_f06_2_posix_stale_readiness_alone_then_real_data() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    connect_pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    io.submit(
        Token(20),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    io.inject_stale(Sock(1), false);
    io.tick(false, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "stale readiness alone must leave Recv pending"
    );

    write_pool_bytes(&mut io, BufId(1), 0, b"arrived");
    io.submit(
        Token(21),
        Op::Send {
            sock: Sock(2),
            buf: BufId(1),
            off: 0,
            len: 7,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 100, |v| {
        v.iter().any(|c| c.token == Token(20))
    })
    .unwrap();
    assert!(cs.iter().any(|c| c.token == Token(20) && c.result == 7));
}

/// Ticks into a one-slot `Completions` until `n` completions arrived or 5 s
/// passed, and returns them.
///
/// Each tick can hand over one completion. The kernel may report the
/// sockets ready a little late under load, so the loop runs to a deadline
/// rather than a fixed number of ticks; a completion that was lost still
/// fails the test, after the deadline.
#[cfg(unix)]
fn collect_one_per_tick<IO: Io>(io: &mut IO, n: usize) -> Vec<longshore::io::Completion> {
    let mut small_out = Completions::new(1);
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while got.len() < n && std::time::Instant::now() < deadline {
        io.tick(false, &mut small_out).unwrap();
        let _: usize = drain_completions(&mut small_out, &mut got);
        if got.len() < n {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    got
}

#[cfg(unix)]
#[test]
fn t2_f06_3_posix_full_completions_buffer_preserves_ready_ops() {
    let limits = Limits {
        sockets_max: 6,
        buffers: 4,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut setup_out = Completions::new(8);
    connect_pair(&mut io, &mut setup_out, Sock(0), Sock(1), Sock(2));
    connect_pair(&mut io, &mut setup_out, Sock(3), Sock(4), Sock(5));

    write_pool_bytes(&mut io, BufId(0), 0, b"msg1");
    write_pool_bytes(&mut io, BufId(1), 0, b"msg2");
    for (tok, sock, buf) in [(1, Sock(2), BufId(0)), (2, Sock(5), BufId(1))] {
        let _: longshore::io::Completion = submit_and_wait_one(
            &mut io,
            &mut setup_out,
            Token(tok),
            Op::Send {
                sock,
                buf,
                off: 0,
                len: 4,
                zero_copy: false,
            },
            100,
        )
        .unwrap();
    }

    // Queue 4 ready operations while Completions capacity is 1.
    for (tok, sock, buf) in [(10, Sock(1), BufId(2)), (11, Sock(4), BufId(3))] {
        io.submit(
            Token(tok),
            Op::Recv {
                sock,
                buf,
                off: 0,
                len: 4,
            },
        )
        .unwrap();
    }
    for (tok, sock, buf) in [(12, Sock(2), BufId(0)), (13, Sock(5), BufId(1))] {
        io.submit(
            Token(tok),
            Op::Send {
                sock,
                buf,
                off: 0,
                len: 4,
                zero_copy: false,
            },
        )
        .unwrap();
    }

    let mut got = collect_one_per_tick(&mut io, 4);
    got.sort_by_key(|c| c.token.0);
    assert_eq!(
        got.iter()
            .map(|c| (c.token.0, c.result))
            .collect::<Vec<_>>(),
        vec![(10, 4), (11, 4), (12, 4), (13, 4)]
    );
}

#[cfg(unix)]
#[test]
fn t2_f06_4_posix_continuous_immediate_ops_do_not_starve_recv() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    connect_pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    write_pool_bytes(&mut io, BufId(0), 0, b"data");
    let _: longshore::io::Completion = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(1),
        Op::Send {
            sock: Sock(2),
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
        100,
    )
    .unwrap();

    io.submit(
        Token(2),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();

    let mut recv_done = false;
    for k in 0..64u64 {
        io.submit(Token(100 + k), Op::Timeout { nanos: 0 }).unwrap();
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(2) {
                assert_eq!(c.result, 4);
                recv_done = true;
            }
        }
        if recv_done {
            break;
        }
    }
    assert!(
        recv_done,
        "Recv starved by continuous immediate submissions"
    );
}

#[cfg(unix)]
#[test]
fn t2_f06_5_posix_linux_errno_mapping_and_waker_outlives_backend() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    connect_pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    // Verify Linux-mapped -EAGAIN (-11), not macOS 35.
    let again = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(1),
        Op::RecvNow {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 16,
        },
        100,
    )
    .unwrap();
    assert_eq!(again.result, -EAGAIN);

    // Close peer and send repeatedly until Linux -EPIPE (-32) or -ECONNRESET (-104).
    let _: longshore::io::Completion = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(2),
        Op::Close { sock: Sock(2) },
        100,
    )
    .unwrap();
    let mut err_res = None;
    for k in 0..16u64 {
        write_pool_bytes(&mut io, BufId(0), 0, b"probe");
        let s = submit_and_wait_one(
            &mut io,
            &mut out,
            Token(10 + k),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 5,
                zero_copy: false,
            },
            100,
        )
        .unwrap();
        if s.result < 0 {
            err_res = Some(s.result);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let code = err_res.expect("closed peer send must error");
    assert!(
        code == -EPIPE || code == -ECONNRESET,
        "expected Linux -EPIPE (-32) or -ECONNRESET (-104), got {code}"
    );

    // Waker must outlive Posix backend safely.
    let waker = io.waker().expect("waker");
    drop(io);
    waker.wake();
    drop(waker);
}
