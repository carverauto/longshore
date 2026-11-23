// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The self-checks `echo_uring` runs on the ring before the echo.

use crate::die::{self, OrDie};
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Busy, Completions, FileId, Io, Op, Sock, Token, ECANCELED};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Submit `op` as token 1 and wait for its completion's result.
fn run_one(io: &mut UringIo, out: &mut Completions, op: Op) -> i32 {
    io.submit(Token::new(1), op).or_die("submit");
    loop {
        io.tick(true, out).or_die("tick");
        if let Some(c) = out.pop() {
            break c.result();
        }
    }
}

/// Read up to 64 bytes of `file` into buffer 0, returning the result.
fn read_file(io: &mut UringIo, out: &mut Completions, file: FileId) -> i32 {
    run_one(
        io,
        out,
        Op::ReadFile {
            file,
            buf: BufId::new(0),
            off: 0,
            len: 64,
        },
    )
}

/// Timers fire shortest first, and a fifth operation is refused.
pub(crate) fn timers(io: &mut UringIo, out: &mut Completions) {
    let t0 = io.now_ns();
    for t in 0..4 {
        io.submit(
            Token::new(t),
            Op::Timeout {
                nanos: 20_000_000 * (4 - t),
            },
        )
        .or_die("submit");
    }
    assert_eq!(
        io.submit(Token::new(9), Op::Timeout { nanos: 1 }),
        Err(Busy),
        "fifth operation must be refused"
    );
    let mut order = [0u64; 4];
    let mut n = 0;
    while n < 4 {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            assert_eq!(c.result(), 0, "timeout result");
            order[n] = c.token().get();
            n += 1;
        }
    }
    let ms = (io.now_ns() - t0) / 1_000_000;
    assert_eq!(order, [3, 2, 1, 0], "timers must fire shortest first");
    assert!((80..200).contains(&ms), "80 ms timer took {ms} ms");
    println!("ok timers order={order:?} elapsed_ms={ms}");
}

/// File reads: open, read and close as one operation.
pub(crate) fn files(io: &mut UringIo, out: &mut Completions) {
    let path = format!("/tmp/longshore-echo-uring-{}.txt", std::process::id());
    let missing = io
        .register_file("/nonexistent/longshore-file")
        .or_die("register_file");
    let file = io.register_file(&path).or_die("register_file");
    assert_eq!(
        read_file(io, out, missing),
        -2,
        "a missing file reports ENOENT"
    );
    for contents in ["first", "second, longer version"] {
        std::fs::write(&path, contents).or_die("write");
        assert_eq!(
            read_file(io, out, file),
            i32::try_from(contents.len()).or_die("file length")
        );
        assert_eq!(
            &io.buf(BufId::new(0))[..contents.len()],
            contents.as_bytes(),
            "file contents"
        );
    }
    std::fs::remove_file(&path).or_die("remove_file");
    println!("ok files: missing, read, reread after replacement");
}

/// Cancelling an operation of several steps. A connect is a socket step
/// then a connect step, and a file read is open, read, close. Submitted
/// together with its cancel, the first step has finished by the time the
/// kernel sees the cancel, so the cancel misses. The operation must stop
/// anyway instead of starting a step nothing will cancel: a connect to an
/// address that never answers would otherwise hang for minutes.
pub(crate) fn cancel(io: &mut UringIo, out: &mut Completions) {
    let (r, ms) = cancel_connect(io, out);
    cancel_read(io, out);
    println!("ok cancel: connect ended with {r} after {ms} ms, file read cancelled before reading");
}

/// A connect cancelled as it starts ends; returns its result and duration.
fn cancel_connect(io: &mut UringIo, out: &mut Completions) -> (i32, u64) {
    let t0 = io.now_ns();
    // TEST-NET-1 (RFC 5737): never routed, so a connect to it gets no answer.
    io.submit(
        Token::new(1),
        Op::Connect {
            sock: Sock::new(3),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9),
        },
    )
    .or_die("submit");
    io.submit(
        Token::new(2),
        Op::Cancel {
            target: Token::new(1),
        },
    )
    .or_die("submit");
    io.submit(
        Token::new(3),
        Op::Timeout {
            nanos: 3_000_000_000,
        },
    )
    .or_die("submit");
    let mut connect = None;
    let mut done = 0u32;
    while connect.is_none() {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            done |= 1 << c.token().get();
            match c.token().get() {
                1 => connect = Some(c.result()),
                3 => die::die("a cancelled connect was still running after 3 s"),
                _ => {}
            }
        }
    }
    // Unreachable networks fail the connect at once, which is also fine.
    let r = connect.or_die("connect");
    assert!(
        r < 0,
        "a cancelled connect to an unanswering address reported success"
    );
    let ms = (io.now_ns() - t0) / 1_000_000;
    // Close the socket the first step created, and let the guard timer and
    // the cancel finish, so the echo starts with every slot free.
    io.submit(Token::new(4), Op::Close { sock: Sock::new(3) })
        .or_die("submit");
    io.submit(
        Token::new(5),
        Op::Cancel {
            target: Token::new(3),
        },
    )
    .or_die("submit");
    let all = 0b11_1110;
    while done != all {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            assert!(
                all & (1 << c.token().get()) != 0 && done & (1 << c.token().get()) == 0,
                "unexpected completion {:?}",
                c.token()
            );
            done |= 1 << c.token().get();
        }
    }
    (r, ms)
}

/// The same for a file read: cancelled as it starts, it must not read.
fn cancel_read(io: &mut UringIo, out: &mut Completions) {
    let path = format!(
        "/tmp/longshore-echo-uring-cancel-{}.txt",
        std::process::id()
    );
    std::fs::write(&path, "must not be read").or_die("write");
    let file = io.register_file(&path).or_die("register_file");
    io.buf(BufId::new(0))[..4].copy_from_slice(b"----");
    io.submit(
        Token::new(1),
        Op::ReadFile {
            file,
            buf: BufId::new(0),
            off: 0,
            len: 64,
        },
    )
    .or_die("submit");
    io.submit(
        Token::new(2),
        Op::Cancel {
            target: Token::new(1),
        },
    )
    .or_die("submit");
    let mut read = None;
    let mut pending = 2;
    while pending > 0 {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == 1 {
                read = Some(c.result());
            }
            pending -= 1;
        }
    }
    std::fs::remove_file(&path).or_die("remove_file");
    assert_eq!(
        read,
        Some(-ECANCELED),
        "a file read cancelled as it started"
    );
    assert_eq!(
        &io.buf(BufId::new(0))[..4],
        b"----",
        "the cancelled read still filled the buffer"
    );
}

/// A connect that fails must leave its socket slot free: the socket the
/// first step created is closed before the failure is reported. A second
/// connect on the same slot then fails the same way, for the same reason,
/// instead of tripping over the first one's socket.
pub(crate) fn failed_connect(io: &mut UringIo, out: &mut Completions, loopback: IpAddr) {
    let mut results = [0i32; 2];
    for r in results.iter_mut() {
        // Port 1 on loopback: nothing listens there.
        *r = run_one(
            io,
            out,
            Op::Connect {
                sock: Sock::new(3),
                addr: SocketAddr::new(loopback, 1),
            },
        );
    }
    assert_eq!(
        results,
        [-111, -111],
        "two refused connects on one socket slot"
    );
    println!("ok failed connect releases its socket slot");
}

/// With measuring on, a connect reports the connection's maximum segment
/// size, and neither a connect that works nor one that is refused leaves
/// the process holding a descriptor. Measuring is then left as `measure`.
pub(crate) fn measured_connect(
    io: &mut UringIo,
    out: &mut Completions,
    loopback: IpAddr,
    measure: bool,
) {
    let open_fds = || {
        std::fs::read_dir("/proc/self/fd")
            .or_die("read_dir")
            .count()
    };
    let before = open_fds();
    io.measure_segment_sizes(true);
    let listening = io
        .listen(Sock::new(0), SocketAddr::new(loopback, 0))
        .or_die("listen");
    let size = run_one(
        io,
        out,
        Op::Connect {
            sock: Sock::new(3),
            addr: listening,
        },
    );
    let segment = longshore::io::Segment::decode(size)
        .unwrap_or_else(|| die::die(format_args!("measured connect returned {size}")));
    // Loopback's MTU is 65536, so its segments are far above Ethernet's.
    assert!(
        segment.size() > 16_000 && segment.path_limit() > 60_000,
        "{segment:?}"
    );
    assert_eq!(run_one(io, out, Op::Close { sock: Sock::new(3) }), 0);
    let refused = run_one(
        io,
        out,
        Op::Connect {
            sock: Sock::new(3),
            addr: SocketAddr::new(loopback, 1),
        },
    );
    assert_eq!(refused, -111, "refused connect with measuring on");
    assert_eq!(run_one(io, out, Op::Close { sock: Sock::new(0) }), 0);
    io.measure_segment_sizes(measure);
    assert_eq!(open_fds(), before, "a measured connect leaked a descriptor");
    println!(
        "ok measured connect: segment size {}, path limit {}, no descriptor left open",
        segment.size(),
        segment.path_limit()
    );
}
