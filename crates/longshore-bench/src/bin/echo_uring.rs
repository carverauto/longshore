// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the echo scenario on real io_uring over loopback, with allocation
//! forbidden after setup. This is the real-kernel counterpart of the
//! simulator's echo test.
//!
//!   echo_uring [--total BYTES] [--chunk BYTES] [--zc] [--v6]
//!
//! `--v6` runs the echo and the refused connect over the IPv6 loopback.
//! `--measure` runs the echo with segment sizes measured on connect.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("echo_uring needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

#[cfg(target_os = "linux")]
fn main() {
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::UringIo;
    use longshore::io::{Completions, Limits};
    use longshore::sim::scenario::echo;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let (mut total, mut chunk, mut zc) = (64u64 << 20, 1u32 << 20, false);
    let mut v6 = false;
    let mut measure = false;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--zc" => zc = true,
            "--v6" => v6 = true,
            "--measure" => measure = true,
            "--total" => {
                total = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--total BYTES")
            }
            "--chunk" => {
                chunk = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--chunk BYTES")
            }
            _ => panic!("unknown flag {k}"),
        }
    }
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: chunk,
        small_buffers: 0,
        small_buffer_size: 0,
        ops_max: 4,
    };
    let mut io = match UringIo::new(limits) {
        Ok(io) => io,
        Err(e) => {
            eprintln!("echo_uring: {e}");
            std::process::exit(1);
        }
    };
    let mut out = Completions::new(8);
    // Timer and operation-limit self-checks before the echo.
    {
        use longshore::io::{Busy, Io, Op, Token};
        let t0 = io.now_ns();
        for t in 0..4 {
            io.submit(
                Token(t),
                Op::Timeout {
                    nanos: 20_000_000 * (4 - t),
                },
            )
            .unwrap();
        }
        assert_eq!(
            io.submit(Token(9), Op::Timeout { nanos: 1 }),
            Err(Busy),
            "fifth operation must be refused"
        );
        let mut order = [0u64; 4];
        let mut n = 0;
        while n < 4 {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert_eq!(c.result, 0, "timeout result");
                order[n] = c.token.0;
                n += 1;
            }
        }
        let ms = (io.now_ns() - t0) / 1_000_000;
        assert_eq!(order, [3, 2, 1, 0], "timers must fire shortest first");
        assert!((80..200).contains(&ms), "80 ms timer took {ms} ms");
        println!("ok timers order={order:?} elapsed_ms={ms}");
    }
    // File reads: open, read and close as one operation.
    {
        use longshore::io::{BufId, Io, Op, Token};
        let path = format!("/tmp/longshore-echo-uring-{}.txt", std::process::id());
        let missing = io.register_file("/nonexistent/longshore-file").unwrap();
        let file = io.register_file(&path).unwrap();
        let mut read = |io: &mut UringIo, f| {
            io.submit(
                Token(1),
                Op::ReadFile {
                    file: f,
                    buf: BufId(0),
                    off: 0,
                    len: 64,
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
        assert_eq!(read(&mut io, missing), -2, "a missing file reports ENOENT");
        for contents in ["first", "second, longer version"] {
            std::fs::write(&path, contents).unwrap();
            assert_eq!(read(&mut io, file), contents.len() as i32);
            assert_eq!(
                &io.buf(BufId(0))[..contents.len()],
                contents.as_bytes(),
                "file contents"
            );
        }
        std::fs::remove_file(&path).unwrap();
        println!("ok files: missing, read, reread after replacement");
    }
    // Cancelling an operation of several steps. A connect is a socket step
    // then a connect step, and a file read is open, read, close. Submitted
    // together with its cancel, the first step has finished by the time the
    // kernel sees the cancel, so the cancel misses. The operation must stop
    // anyway instead of starting a step nothing will cancel: a connect to an
    // address that never answers would otherwise hang for minutes.
    {
        use longshore::io::{BufId, Io, Op, Sock, Token, ECANCELED};
        let t0 = io.now_ns();
        // TEST-NET-1 (RFC 5737): never routed, so a connect to it gets no answer.
        io.submit(
            Token(1),
            Op::Connect {
                sock: Sock(3),
                addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9),
            },
        )
        .unwrap();
        io.submit(Token(2), Op::Cancel { target: Token(1) })
            .unwrap();
        io.submit(
            Token(3),
            Op::Timeout {
                nanos: 3_000_000_000,
            },
        )
        .unwrap();
        let mut connect = None;
        let mut done = 0u32;
        while connect.is_none() {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                done |= 1 << c.token.0;
                match c.token.0 {
                    1 => connect = Some(c.result),
                    3 => panic!("a cancelled connect was still running after 3 s"),
                    _ => {}
                }
            }
        }
        // Unreachable networks fail the connect at once, which is also fine.
        let r = connect.unwrap();
        assert!(
            r < 0,
            "a cancelled connect to an unanswering address reported success"
        );
        let ms = (io.now_ns() - t0) / 1_000_000;
        // Close the socket the first step created, and let the guard timer and
        // the cancel finish, so the echo starts with every slot free.
        io.submit(Token(4), Op::Close { sock: Sock(3) }).unwrap();
        io.submit(Token(5), Op::Cancel { target: Token(3) })
            .unwrap();
        let all = 0b11_1110;
        while done != all {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert!(
                    all & (1 << c.token.0) != 0 && done & (1 << c.token.0) == 0,
                    "unexpected completion {:?}",
                    c.token
                );
                done |= 1 << c.token.0;
            }
        }

        // The same for a file read: cancelled as it starts, it must not read.
        let path = format!(
            "/tmp/longshore-echo-uring-cancel-{}.txt",
            std::process::id()
        );
        std::fs::write(&path, "must not be read").unwrap();
        let file = io.register_file(&path).unwrap();
        io.buf(BufId(0))[..4].copy_from_slice(b"----");
        io.submit(
            Token(1),
            Op::ReadFile {
                file,
                buf: BufId(0),
                off: 0,
                len: 64,
            },
        )
        .unwrap();
        io.submit(Token(2), Op::Cancel { target: Token(1) })
            .unwrap();
        let mut read = None;
        let mut pending = 2;
        while pending > 0 {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token.0 == 1 {
                    read = Some(c.result);
                }
                pending -= 1;
            }
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            read,
            Some(-ECANCELED),
            "a file read cancelled as it started"
        );
        assert_eq!(
            &io.buf(BufId(0))[..4],
            b"----",
            "the cancelled read still filled the buffer"
        );
        println!(
            "ok cancel: connect ended with {r} after {ms} ms, file read cancelled before reading"
        );
    }
    let loopback = if v6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    // A connect that fails must leave its socket slot free: the socket the
    // first step created is closed before the failure is reported. A second
    // connect on the same slot then fails the same way, for the same reason,
    // instead of tripping over the first one's socket.
    {
        use longshore::io::{Io, Op, Sock, Token};
        let mut results = [0i32; 2];
        for r in results.iter_mut() {
            // Port 1 on loopback: nothing listens there.
            io.submit(
                Token(1),
                Op::Connect {
                    sock: Sock(3),
                    addr: SocketAddr::new(loopback, 1),
                },
            )
            .unwrap();
            *r = loop {
                io.tick(true, &mut out).unwrap();
                if let Some(c) = out.pop() {
                    break c.result;
                }
            };
        }
        assert_eq!(
            results,
            [-111, -111],
            "two refused connects on one socket slot"
        );
        println!("ok failed connect releases its socket slot");
    }
    // With measuring on, a connect reports the connection's maximum segment
    // size, and neither a connect that works nor one that is refused leaves
    // the process holding a descriptor.
    {
        use longshore::io::{Io, Op, Sock, Token};
        let open_fds = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let mut run = |io: &mut UringIo, op: Op| {
            io.submit(Token(1), op).unwrap();
            loop {
                io.tick(true, &mut out).unwrap();
                if let Some(c) = out.pop() {
                    break c.result;
                }
            }
        };
        let before = open_fds();
        io.measure_segment_sizes(true);
        let listening = io.listen(Sock(0), SocketAddr::new(loopback, 0)).unwrap();
        let size = run(
            &mut io,
            Op::Connect {
                sock: Sock(3),
                addr: listening,
            },
        );
        // Loopback's MTU is 65536, so its segments are far above Ethernet's.
        assert!(size > 16_000, "measured connect returned {size}");
        assert_eq!(run(&mut io, Op::Close { sock: Sock(3) }), 0);
        let refused = run(
            &mut io,
            Op::Connect {
                sock: Sock(3),
                addr: SocketAddr::new(loopback, 1),
            },
        );
        assert_eq!(refused, -111, "refused connect with measuring on");
        assert_eq!(run(&mut io, Op::Close { sock: Sock(0) }), 0);
        io.measure_segment_sizes(measure);
        assert_eq!(open_fds(), before, "a measured connect leaked a descriptor");
        println!("ok measured connect: segment size {size}, no descriptor left open");
    }
    let addr = SocketAddr::new(loopback, 0);
    let start = std::time::Instant::now();
    let r = sealed(|| echo(&mut io, addr, total, chunk, zc, &mut out));
    let secs = start.elapsed().as_secs_f64();
    match r {
        Ok(s) => println!("ok total={total} chunk={chunk} zero_copy={zc} sends={} recvs={} ticks={} enters={} secs={secs:.3}", s.sends, s.recvs, s.ticks, io.enters()),
        Err(e) => {
            eprintln!("echo_uring: FAILED {e:?}");
            std::process::exit(1);
        }
    }
}
