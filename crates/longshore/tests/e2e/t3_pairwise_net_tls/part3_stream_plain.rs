// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-16`–`T3-17`: `longshore_stream` plain/TLS
//! over armed buffer ring, and `--no-default-features` sockopts + TCP/Unix echo.

use super::super::harness::{drive_until, read_pool_bytes, write_nul_path, write_pool_bytes};
use super::net_limits;
use longshore::io::{BufId, Completions, Io, Op, Sock, Token};
use longshore::stream::{Stream, TlsError};

/// Four plain connections sit behind armed recvs; none may hold a ring buffer while idle.
#[cfg(feature = "sim")]
fn plain_idle_connections_hold_no_ring_buffers() {
    use longshore::sim::{Faults, SimIo};

    let limits = net_limits(20, 10, 8, 4, 1024);
    let mut sim = SimIo::new(limits, 316, Faults::default());
    let mut out = Completions::new(32);
    let addr = sim
        .listen(Sock::new(0), "192.0.2.36:9316".parse().unwrap())
        .unwrap();
    sim.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();

    for i in 0..4u32 {
        sim.submit(
            Token::new(10 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut sim, &mut out, 1000, |v| {
        v.iter().filter(|c| c.token().get() == 1).count() == 4
            && v.iter()
                .filter(|c| (10..14).contains(&c.token().get()))
                .count()
                == 4
    })
    .unwrap();
    for (idx, c) in cs.iter().filter(|c| c.token().get() == 1).enumerate() {
        sim.submit(
            Token::new(100 + idx as u64),
            Op::ArmedRecv {
                sock: Sock::new(u32::try_from(c.result()).unwrap()),
            },
        )
        .unwrap();
    }
    sim.tick(false, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "idle plain connections must hold 0 ring buffers"
    );
}

/// A TLS client and server `Stream` over a connected simulated pair.
#[cfg(all(feature = "sim", feature = "tls-core"))]
fn tls_stream_pair(
    tls_limits: longshore::io::Limits,
) -> (longshore::sim::SimIo, Completions, Stream, Stream) {
    use longshore::sim::{Faults, SimIo};

    let mut tsim = SimIo::new(tls_limits, 3160, Faults::default());
    let mut tout = Completions::new(32);
    let taddr = tsim
        .listen(Sock::new(0), "192.0.2.37:9317".parse().unwrap())
        .unwrap();
    tsim.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    tsim.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr: taddr,
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut tsim, &mut tout, 1000, |v| v.len() == 2).unwrap();

    let cap = tls_limits.buffer_size();
    let cli = Stream::new(Sock::new(1), Token::new(10), Token::new(11)).with_tls_buffers(
        BufId::new(0),
        BufId::new(1),
        cap,
        cap,
        u32::MAX,
    );
    let srv = Stream::new(Sock::new(2), Token::new(20), Token::new(21)).with_tls_buffers(
        BufId::new(2),
        BufId::new(3),
        cap,
        cap,
        u32::MAX,
    );
    (tsim, tout, cli, srv)
}

/// Drives both handshakes for up to 500 ticks; returns whether each side finished.
#[cfg(all(feature = "sim", feature = "tls-core"))]
fn drive_handshakes(
    tsim: &mut longshore::sim::SimIo,
    tout: &mut Completions,
    cli: &mut Stream,
    srv: &mut Stream,
) -> (bool, bool) {
    use longshore::stream::Step;

    let (mut cli_done, mut srv_done) = (false, false);
    for _ in 0..500 {
        if cli_done && srv_done {
            break;
        }
        tsim.tick(true, tout).unwrap();
        while let Some(c) = tout.pop() {
            let step = match c.token().get() {
                10 => cli.on_handshake(tsim, true, c.result()),
                11 => cli.on_handshake(tsim, false, c.result()),
                20 => srv.on_handshake(tsim, true, c.result()),
                21 => srv.on_handshake(tsim, false, c.result()),
                _ => continue,
            };
            if matches!(step, Step::Done(0)) {
                if c.token().get() < 20 {
                    cli_done = true;
                } else {
                    srv_done = true;
                }
            }
        }
    }
    (cli_done, srv_done)
}

/// A TLS client and server complete their handshake over the simulator.
#[cfg(all(feature = "sim", feature = "tls-core"))]
fn tls_streams_complete_handshake() {
    use super::super::harness::make_tls_identity;
    use longshore::stream::Step;
    use longshore::tls::handshake::Handshake;
    use longshore::tls::record::{FRAGMENT_MAX, OVERHEAD};

    let id = make_tls_identity(&["localhost"]);
    let mut tls_limits = net_limits(20, 10, 8, 4, 1024);
    tls_limits = tls_limits.with_buffers(6);
    tls_limits = tls_limits.with_buffer_size((2 * (FRAGMENT_MAX + OVERHEAD + 256) + 1024) as u32);
    let (mut tsim, mut tout, mut cli, mut srv) = tls_stream_pair(tls_limits);
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

    let (cli_done, srv_done) = drive_handshakes(&mut tsim, &mut tout, &mut cli, &mut srv);
    assert!(cli_done && srv_done && cli.is_tls() && srv.is_tls());
}

#[cfg(feature = "sim")]
#[test]
fn t3_16_stream_tls_vs_plain_armed_buffer_ring_invariant() {
    plain_idle_connections_hold_no_ring_buffers();
    #[cfg(feature = "tls-core")]
    tls_streams_complete_handshake();
}

/// Connects a TCP pair (`Sock(1)` to `Sock(2)`) and a Unix pair (`Sock(4)`
/// to `Sock(5)`) and checks all four sides succeed.
#[cfg(unix)]
fn connect_tcp_and_unix<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    limits: longshore::io::Limits,
    sock_p: &str,
) {
    let tcp_addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    io.listen_unix(Sock::new(3), sock_p).unwrap();

    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr: tcp_addr,
        },
    )
    .unwrap();
    let up_len = write_nul_path(io, limits.small(0), 0, sock_p);
    io.submit(
        Token::new(3),
        Op::Accept {
            listener: Sock::new(3),
            sock: Sock::new(5),
        },
    )
    .unwrap();
    io.submit(
        Token::new(4),
        Op::ConnectUnix {
            sock: Sock::new(4),
            path_buf: limits.small(0),
            path_off: 0,
            path_len: up_len,
        },
    )
    .unwrap();
    let cs = drive_until(io, out, 10_000, |v| v.len() == 4).unwrap();
    assert!(cs.iter().all(|c| c.result() == 0));
}

/// Receives up to 16 bytes on `sock` into `buf`; returns what arrived.
#[cfg(unix)]
fn recv_bytes<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    sock: Sock,
    buf: BufId,
) -> Vec<u8> {
    use super::super::harness::submit_and_wait_one;
    let r = submit_and_wait_one(
        io,
        out,
        token,
        Op::Recv {
            sock,
            buf,
            off: 0,
            len: 16,
        },
        10_000,
    )
    .unwrap();
    read_pool_bytes(io, buf, 0, usize::try_from(r.result()).unwrap())
}

#[test]
fn t3_17_plain_build_sockopts_tcp_and_unix_stream_echo() {
    let err_sample: Option<TlsError> = None;
    assert!(err_sample.is_none());

    #[cfg(unix)]
    {
        use super::super::harness::{posix_backend, TempDir};

        let dir = TempDir::new("t3_17").unwrap();
        let sock_p = dir.socket_path("plain.sock");
        let limits = net_limits(8, 4, 2, 2, 1024);
        let mut io = posix_backend(limits).unwrap();
        io.no_delay(true);
        io.keep_alive(true);
        let mut out = Completions::new(16);
        connect_tcp_and_unix(&mut io, &mut out, limits, sock_p.to_str().unwrap());

        let mut tcp_s = Stream::new(Sock::new(1), Token::new(10), Token::new(11));
        let mut unix_s = Stream::new(Sock::new(4), Token::new(12), Token::new(13));
        assert!(!tcp_s.is_tls() && !unix_s.is_tls());
        write_pool_bytes(&mut io, BufId::new(0), 0, b"tcp-plain");
        write_pool_bytes(&mut io, BufId::new(1), 0, b"unix-plain");
        tcp_s.send(&mut io, BufId::new(0), 0, 9, u32::MAX);
        unix_s.send(&mut io, BufId::new(1), 0, 10, u32::MAX);
        let r1 = recv_bytes(
            &mut io,
            &mut out,
            Token::new(20),
            Sock::new(2),
            BufId::new(2),
        );
        let r2 = recv_bytes(
            &mut io,
            &mut out,
            Token::new(21),
            Sock::new(5),
            BufId::new(3),
        );
        assert_eq!(r1, b"tcp-plain");
        assert_eq!(r2, b"unix-plain");
    }
}
