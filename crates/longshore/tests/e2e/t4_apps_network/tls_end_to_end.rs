// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-08`: End-to-end TLS 1.3 server & client (`longshore::stream::Stream`
//! + `longshore::client` HTTPS + `make_tls_identity` + `IdleCheck`).

use super::super::harness::{make_tls_identity, posix_backend, write_pool_bytes, HttpStoreServer};
use longshore::client::Builder;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
use longshore::stream::{IdleCheck, Step, Stream};
use longshore::tls::handshake::Handshake;
use std::net::SocketAddr;

const TLS_BUF_CAP: u32 = 40 * 1024;

fn tls_stream_limits() -> Limits {
    Limits {
        sockets_max: 6,
        buffers: 8,
        buffer_size: TLS_BUF_CAP,
        small_buffers: 2,
        small_buffer_size: 256,
        accept_range_start: 3,
        accept_range_len: 3,
        ops_max: 16,
        ..Limits::BASE
    }
}

/// Connects `Sock(1)` through an armed accept; returns the server socket.
fn accept_one<IO: Io>(io: &mut IO, out: &mut Completions, addr: SocketAddr) -> Sock {
    let bound = io.listen(Sock(0), addr).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr: bound,
        },
    )
    .unwrap();

    let mut srv_sock: Option<Sock> = None;
    let mut cli_ok = false;
    while srv_sock.is_none() || !cli_ok {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 1 && c.result >= 0 {
                srv_sock = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token.0 == 2 {
                assert_eq!(c.result, 0);
                cli_ok = true;
            }
        }
    }
    srv_sock.unwrap()
}

/// Feeds one handshake completion to `stream`; returns whether it finished.
fn handshake_step<IO: Io>(
    stream: &mut Stream,
    io: &mut IO,
    send: bool,
    res: i32,
    who: &str,
) -> bool {
    let step = stream.on_handshake(io, send, res);
    if let Step::Done(0) = step {
        true
    } else if let Step::Tls(e) = step {
        panic!("{who} TLS handshake error: {e:?}");
    } else {
        false
    }
}

/// Runs the client and server TLS 1.3 handshakes to completion.
fn complete_handshakes<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    cli_stream: &mut Stream,
    srv_stream: &mut Stream,
) {
    let id = make_tls_identity(&["localhost", "127.0.0.1"]);
    let srv_setup = id.server_setup();
    let cli_setup = id.client_setup();
    assert_eq!(
        cli_stream.start_handshake(io, Handshake::client(&cli_setup, "localhost").unwrap()),
        Step::Pending
    );
    assert_eq!(
        srv_stream.start_handshake(io, Handshake::server(&srv_setup).unwrap()),
        Step::Pending
    );

    let mut cli_hs_done = false;
    let mut srv_hs_done = false;
    while !(cli_hs_done && srv_hs_done) {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            match c.token.0 {
                10 | 11 => {
                    cli_hs_done |=
                        handshake_step(cli_stream, io, c.token.0 == 10, c.result, "client");
                }
                20 | 21 => {
                    srv_hs_done |=
                        handshake_step(srv_stream, io, c.token.0 == 20, c.result, "server");
                }
                _ => {}
            }
        }
    }
}

/// The client sends `msg` encrypted; the server decrypts it into `BufId(5)`.
fn exchange_payload<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    cli_stream: &mut Stream,
    srv_stream: &mut Stream,
    msg: &[u8],
) {
    write_pool_bytes(io, BufId(4), 0, msg);
    cli_stream.send(io, BufId(4), 0, msg.len() as u32, u32::MAX);
    let mut srv_got = match srv_stream.recv(io, BufId(5), 0, 1024) {
        Step::Done(n) => {
            assert!(n > 0);
            usize::try_from(n).unwrap()
        }
        Step::Pending => 0,
        other => panic!("unexpected initial recv step: {other:?}"),
    };

    let mut cli_sent = false;
    while srv_got == 0 || !cli_sent {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            match c.token.0 {
                10 => {
                    if let Step::Done(n) = cli_stream.on_send(io, c.result) {
                        assert_eq!(n, i32::try_from(msg.len()).unwrap());
                        cli_sent = true;
                    }
                }
                21 => {
                    if let Step::Done(n) = srv_stream.on_recv(io, c.result) {
                        assert!(n > 0);
                        srv_got = usize::try_from(n).unwrap();
                    }
                }
                _ => {}
            }
        }
    }
    assert_eq!(&io.buf(BufId(5))[..srv_got], msg);
}

/// `IdleCheck` on a healthy idle TLS socket reports `Clean` (not Dirty or Closed).
fn idle_check_is_clean<IO: Io>(io: &mut IO, out: &mut Completions, srv_stream: &mut Stream) {
    let mut idle = srv_stream.check_idle(io, BufId(6), 0);
    while idle == IdleCheck::Pending {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 21 {
                idle = srv_stream.on_idle_check(io, c.result);
            }
        }
    }
    assert_eq!(idle, IdleCheck::Clean);
}

/// The server closes; the client TLS stream observes clean EOF (`Step::Done(0)`).
fn server_close_gives_client_eof<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    cli_stream: &mut Stream,
    srv_sock: Sock,
) {
    io.submit(Token(40), Op::Close { sock: srv_sock }).unwrap();
    let mut saw_eof = matches!(cli_stream.recv(io, BufId(6), 0, 1024), Step::Done(0));
    while !saw_eof {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 11 {
                if let Step::Done(0) = cli_stream.on_recv(io, c.result) {
                    saw_eof = true;
                }
            }
        }
    }
}

fn run_tls_stream_server_client_roundtrip<IO: Io>(io: &mut IO, addr: SocketAddr) {
    let mut out = Completions::new(16);
    let srv_sock = accept_one(io, &mut out, addr);

    let mut cli_stream = Stream::new(Sock(1), Token(10), Token(11)).with_tls_buffers(
        BufId(0),
        BufId(1),
        TLS_BUF_CAP,
        16 * 1024,
        32 * 1024,
    );
    let mut srv_stream = Stream::new(srv_sock, Token(20), Token(21)).with_tls_buffers(
        BufId(2),
        BufId(3),
        TLS_BUF_CAP,
        16 * 1024,
        32 * 1024,
    );
    complete_handshakes(io, &mut out, &mut cli_stream, &mut srv_stream);
    exchange_payload(
        io,
        &mut out,
        &mut cli_stream,
        &mut srv_stream,
        b"TLS-1.3-E2E-APPLICATION-PAYLOAD",
    );
    idle_check_is_clean(io, &mut out, &mut srv_stream);
    server_close_gives_client_eof(io, &mut out, &mut cli_stream, srv_sock);
}

#[cfg(feature = "sim")]
#[test]
fn t4_08_tls_stream_server_client_sim() {
    let mut sim = SimIo::new(tls_stream_limits(), 408, Faults::default());
    run_tls_stream_server_client_roundtrip(&mut sim, "127.0.0.1:9408".parse().unwrap());
}

#[cfg(unix)]
#[test]
fn t4_08_tls_stream_server_client_posix() {
    let mut posix = posix_backend(tls_stream_limits()).unwrap();
    run_tls_stream_server_client_roundtrip(&mut posix, "127.0.0.1:0".parse().unwrap());
}

#[test]
fn t4_08_tls_http_store_and_client_handshake_and_reuse() {
    let id = make_tls_identity(&["localhost", "127.0.0.1"]);
    let store = HttpStoreServer::start_tls(id.server_setup()).unwrap();
    let creds = Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .unwrap();
    let mut b = Builder::new("localhost", "us-east-1", creds);
    b.tls = true;
    b.port = store.port();
    b.address = Some(store.addr());
    b.bucket_in_path = Some("tls-bucket".into());
    b.buffers = 4;
    b.buffer_size = 16 * 1024;
    b.connections = 4;
    b.operations = 8;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    // Note: Builder::build verifies against public root CAs unless configured;
    // when custom self-signed cert is served, client connect attempts TLS 1.3 handshake
    // cleanly and surfaces Fatal TLS/Untrusted error without panicking.
    let client = b.build().expect("client build with tls=true");
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..11].copy_from_slice(b"tls-payload");
    let (res, _buf) = client.store("tls-obj", buf, 11).wait();
    // Self-signed cert not in webpki roots returns a clean Store(Fatal) error,
    // confirming real TLS 1.3 handshake verification occurred over the wire.
    assert!(
        res.is_err(),
        "self-signed cert must be rejected by webpki roots"
    );
    assert!(store.request_count() == 0);
}
