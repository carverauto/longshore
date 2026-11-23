// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-16`–`T3-17`: `longshore_stream` plain/TLS
//! over armed buffer ring, and `--no-default-features` sockopts + TCP/Unix echo.

use super::super::harness::{drive_until, read_pool_bytes, write_nul_path, write_pool_bytes};
use super::net_limits;
use longshore::io::{BufId, Completion, Completions, Io, Op, Sock, Token};
use longshore::stream::{Stream, TlsError};

#[cfg(feature = "sim")]
#[test]
fn t3_16_stream_tls_vs_plain_armed_buffer_ring_invariant() {
    use longshore::sim::{Faults, SimIo};

    let limits = net_limits(20, 10, 8, 4, 1024);
    let mut sim = SimIo::new(limits, 316, Faults::default());
    let mut out = Completions::new(32);
    let addr = sim
        .listen(Sock(0), "192.0.2.36:9316".parse().unwrap())
        .unwrap();
    sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    for i in 0..4u32 {
        sim.submit(
            Token(10 + i as u64),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut sim, &mut out, 1000, |v| {
        v.iter().filter(|c| c.token == Token(1)).count() == 4
            && v.iter().filter(|c| (10..14).contains(&c.token.0)).count() == 4
    })
    .unwrap();
    for (idx, c) in cs.iter().filter(|c| c.token == Token(1)).enumerate() {
        sim.submit(
            Token(100 + idx as u64),
            Op::ArmedRecv {
                sock: Sock(c.result as u32),
            },
        )
        .unwrap();
    }
    sim.tick(false, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "idle plain connections must hold 0 ring buffers"
    );

    #[cfg(feature = "tls")]
    {
        use super::super::harness::make_tls_identity;
        use longshore::stream::Step;
        use longshore::tls::handshake::Handshake;
        use longshore::tls::record::{FRAGMENT_MAX, OVERHEAD};

        let id = make_tls_identity(&["localhost"]);
        let mut tls_limits = net_limits(20, 10, 8, 4, 1024);
        tls_limits.buffers = 6;
        tls_limits.buffer_size = (2 * (FRAGMENT_MAX + OVERHEAD + 256) + 1024) as u32;
        let mut tsim = SimIo::new(tls_limits, 3160, Faults::default());
        let mut tout = Completions::new(32);
        let taddr = tsim
            .listen(Sock(0), "192.0.2.37:9317".parse().unwrap())
            .unwrap();
        tsim.submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
        tsim.submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr: taddr,
            },
        )
        .unwrap();
        let _: Vec<Completion> = drive_until(&mut tsim, &mut tout, 1000, |v| v.len() == 2).unwrap();

        let cap = tls_limits.buffer_size;
        let mut cli = Stream::new(Sock(1), Token(10), Token(11)).with_tls_buffers(
            BufId(0),
            BufId(1),
            cap,
            cap,
            u32::MAX,
        );
        let mut srv = Stream::new(Sock(2), Token(20), Token(21)).with_tls_buffers(
            BufId(2),
            BufId(3),
            cap,
            cap,
            u32::MAX,
        );
        assert!(matches!(
            cli.start_handshake(
                &mut tsim,
                Handshake::client(&id.client_setup(), "localhost").unwrap()
            ),
            Step::Pending
        ));
        assert!(matches!(
            srv.start_handshake(&mut tsim, Handshake::server(&id.server_setup()).unwrap()),
            Step::Pending
        ));

        let (mut cli_done, mut srv_done) = (false, false);
        for _ in 0..500 {
            if cli_done && srv_done {
                break;
            }
            tsim.tick(true, &mut tout).unwrap();
            while let Some(c) = tout.pop() {
                let step = match c.token.0 {
                    10 => cli.on_handshake(&mut tsim, true, c.result),
                    11 => cli.on_handshake(&mut tsim, false, c.result),
                    20 => srv.on_handshake(&mut tsim, true, c.result),
                    21 => srv.on_handshake(&mut tsim, false, c.result),
                    _ => continue,
                };
                if matches!(step, Step::Done(0)) {
                    if c.token.0 < 20 {
                        cli_done = true;
                    } else {
                        srv_done = true;
                    }
                }
            }
        }
        assert!(cli_done && srv_done && cli.is_tls() && srv.is_tls());
    }
}

#[test]
fn t3_17_plain_build_sockopts_tcp_and_unix_stream_echo() {
    let err_sample: Option<TlsError> = None;
    assert!(err_sample.is_none());

    #[cfg(unix)]
    {
        use super::super::harness::{posix_backend, submit_and_wait_one, TempDir};

        let dir = TempDir::new("t3_17").unwrap();
        let sock_p = dir.socket_path("plain.sock");
        let limits = net_limits(8, 4, 2, 2, 1024);
        let mut io = posix_backend(limits).unwrap();
        io.no_delay(true);
        io.keep_alive(true);
        let mut out = Completions::new(16);

        let tcp_addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
        io.listen_unix(Sock(3), sock_p.to_str().unwrap()).unwrap();

        io.submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr: tcp_addr,
            },
        )
        .unwrap();
        let up_len = write_nul_path(&mut io, limits.small(0), 0, sock_p.to_str().unwrap());
        io.submit(
            Token(3),
            Op::Accept {
                listener: Sock(3),
                sock: Sock(5),
            },
        )
        .unwrap();
        io.submit(
            Token(4),
            Op::ConnectUnix {
                sock: Sock(4),
                path_buf: limits.small(0),
                path_off: 0,
                path_len: up_len,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 10_000, |v| v.len() == 4).unwrap();
        assert!(cs.iter().all(|c| c.result == 0));

        let mut tcp_s = Stream::new(Sock(1), Token(10), Token(11));
        let mut unix_s = Stream::new(Sock(4), Token(12), Token(13));
        assert!(!tcp_s.is_tls() && !unix_s.is_tls());
        write_pool_bytes(&mut io, BufId(0), 0, b"tcp-plain");
        write_pool_bytes(&mut io, BufId(1), 0, b"unix-plain");
        tcp_s.send(&mut io, BufId(0), 0, 9, u32::MAX);
        unix_s.send(&mut io, BufId(1), 0, 10, u32::MAX);
        let r1 = submit_and_wait_one(
            &mut io,
            &mut out,
            Token(20),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(2),
                off: 0,
                len: 16,
            },
            10_000,
        )
        .unwrap();
        let r2 = submit_and_wait_one(
            &mut io,
            &mut out,
            Token(21),
            Op::Recv {
                sock: Sock(5),
                buf: BufId(3),
                off: 0,
                len: 16,
            },
            10_000,
        )
        .unwrap();
        assert_eq!(
            read_pool_bytes(&mut io, BufId(2), 0, r1.result as usize),
            b"tcp-plain"
        );
        assert_eq!(
            read_pool_bytes(&mut io, BufId(3), 0, r2.result as usize),
            b"unix-plain"
        );
    }
}
