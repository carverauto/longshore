// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulator tests for TLS `Stream` on both the connecting and the accepting side.
#![cfg(feature = "tls")]

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_stream::{IdleCheck, Step, Stream, TlsError};
use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(test)]
#[path = "tls/adversarial.rs"]
mod adversarial;
#[cfg(test)]
#[path = "tls/adversarial_records.rs"]
mod adversarial_records;

#[global_allocator]
static ALLOC: Guard = Guard;

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)), 443);
const NAME: &str = "stream.test";
const BUF: u32 = 64 * 1024;

const LISTENER: Sock = Sock(0);
const CLIENT: Sock = Sock(1);
const SERVER: Sock = Sock(2);

const C_CONN: Token = Token(1);
const C_SEND: Token = Token(2);
const C_RECV: Token = Token(3);
const S_ACCEPT: Token = Token(4);
const S_SEND: Token = Token(5);
const S_RECV: Token = Token(6);

const C_TLS_TX: BufId = BufId(0);
const C_TLS_RX: BufId = BufId(1);
const S_TLS_TX: BufId = BufId(2);
const S_TLS_RX: BufId = BufId(3);
const C_TX: BufId = BufId(4);
const C_RX: BufId = BufId(5);
const S_BUF: BufId = BufId(6);
const SCRATCH: BufId = BufId(7);

const LIMITS: Limits = Limits {
    sockets_max: 3,
    buffers: 8,
    buffer_size: BUF,
    ops_max: 8,
    ..Limits::BASE
};

struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

#[cfg(test)]
fn identity(name: &str) -> Identity {
    let k = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    Identity {
        cert: k.cert.der().to_vec(),
        key: k.signing_key.serialize_der(),
    }
}

#[cfg(test)]
fn setup_streams(
    seed: u64,
    faults: Faults,
    client_setup: &ClientSetup,
    server_setup: &ServerSetup,
    server_name: &str,
) -> (SimIo, Stream, Stream, Step, Step) {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let _: SocketAddr = io.listen(LISTENER, ADDR).unwrap();
    io.submit(
        S_ACCEPT,
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )
    .unwrap();
    io.submit(
        C_CONN,
        Op::Connect {
            sock: CLIENT,
            addr: ADDR,
        },
    )
    .unwrap();

    let mut out = Completions::new(8);
    let (mut c_up, mut s_up) = (false, false);
    sealed(|| {
        while !(c_up && s_up) {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                match c.token {
                    C_CONN => {
                        assert!(c.result >= 0);
                        c_up = true;
                    }
                    S_ACCEPT => {
                        assert!(c.result >= 0);
                        s_up = true;
                    }
                    other => panic!("unexpected token {other:?}"),
                }
            }
        }
    });

    let mut client =
        Stream::new(CLIENT, C_SEND, C_RECV).with_tls_buffers(C_TLS_TX, C_TLS_RX, BUF, BUF, 4096);
    let mut server =
        Stream::new(SERVER, S_SEND, S_RECV).with_tls_buffers(S_TLS_TX, S_TLS_RX, BUF, BUF, 4096);

    let c_hs = Handshake::client(client_setup, server_name).unwrap();
    let s_hs = Handshake::server(server_setup).unwrap();

    let mut c_step = Step::Pending;
    let mut s_step = Step::Pending;
    sealed(|| {
        c_step = client.start_handshake(&mut io, c_hs);
        s_step = server.start_handshake(&mut io, s_hs);
        while (matches!(c_step, Step::Pending) || matches!(s_step, Step::Pending))
            && !matches!(c_step, Step::Tls(_))
            && !matches!(s_step, Step::Tls(_))
        {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                match c.token {
                    C_SEND => c_step = client.on_handshake(&mut io, true, c.result),
                    C_RECV => c_step = client.on_handshake(&mut io, false, c.result),
                    S_SEND => s_step = server.on_handshake(&mut io, true, c.result),
                    S_RECV => s_step = server.on_handshake(&mut io, false, c.result),
                    other => panic!("unexpected token {other:?}"),
                }
            }
        }
    });

    (io, client, server, c_step, s_step)
}

fn fill_pattern(buf: &mut [u8], seed: u8) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ seed;
    }
}

#[cfg(test)]
fn drain_recv(
    io: &mut SimIo,
    stream: &mut Stream,
    buf: BufId,
    have: &mut u32,
    total: u32,
    chunk: u32,
    mut step: Step,
) {
    loop {
        match step {
            Step::Pending => return,
            Step::Done(n) => {
                assert!(n > 0);
                *have += u32::try_from(n).unwrap();
                if *have == total {
                    return;
                }
                let ask = chunk.min(total - *have);
                step = stream.recv(io, buf, *have, ask);
            }
            Step::Tls(e) => panic!("tls recv error: {e:?}"),
        }
    }
}

#[cfg(test)]
fn round_trip(
    io: &mut SimIo,
    client: &mut Stream,
    server: &mut Stream,
    len: u32,
    rx_chunk: u32,
    seed: u8,
) {
    fill_pattern(&mut io.buf(C_TX)[..len as usize], seed);
    let expected = io.buf(C_TX)[..len as usize].to_vec();
    io.buf(C_RX).fill(0xA5);

    let mut out = Completions::new(8);
    sealed(|| {
        let (mut s_recvd, mut c_recvd) = (0u32, 0u32);
        let (mut c_sent_done, mut s_sent_done) = (false, false);

        client.send(io, C_TX, 0, len, 4096);
        let step = server.recv(io, S_BUF, 0, len);
        drain_recv(io, server, S_BUF, &mut s_recvd, len, len, step);
        if s_recvd == len {
            server.send(io, S_BUF, 0, len, 4096);
            let client_step = client.recv(io, C_RX, 0, rx_chunk.min(len));
            drain_recv(io, client, C_RX, &mut c_recvd, len, rx_chunk, client_step);
        }

        while !c_sent_done || !s_sent_done || s_recvd < len || c_recvd < len {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                match c.token {
                    C_SEND => match client.on_send(io, c.result) {
                        Step::Pending => {}
                        Step::Done(n) => {
                            assert_eq!(u32::try_from(n).unwrap(), len);
                            c_sent_done = true;
                        }
                        Step::Tls(e) => panic!("client send tls error: {e:?}"),
                    },
                    S_RECV => {
                        let server_step = server.on_recv(io, c.result);
                        drain_recv(io, server, S_BUF, &mut s_recvd, len, len, server_step);
                        if s_recvd == len && !s_sent_done {
                            server.send(io, S_BUF, 0, len, 4096);
                            let client_step = client.recv(io, C_RX, 0, rx_chunk.min(len));
                            drain_recv(io, client, C_RX, &mut c_recvd, len, rx_chunk, client_step);
                        }
                    }
                    S_SEND => match server.on_send(io, c.result) {
                        Step::Pending => {}
                        Step::Done(n) => {
                            assert_eq!(u32::try_from(n).unwrap(), len);
                            s_sent_done = true;
                        }
                        Step::Tls(e) => panic!("server send tls error: {e:?}"),
                    },
                    C_RECV => {
                        let client_step = client.on_recv(io, c.result);
                        drain_recv(io, client, C_RX, &mut c_recvd, len, rx_chunk, client_step);
                    }
                    other => panic!("unexpected token {other:?}"),
                }
            }
        }
    });

    assert_eq!(&io.buf(C_RX)[..len as usize], &expected[..]);
    assert!(io.buf(C_RX)[len as usize..].iter().all(|b| *b == 0xA5));
}

#[test]
fn client_and_server_tls_streams_handshake_and_echo_across_boundaries() {
    let id = identity(NAME);
    let client_setup = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let server_setup = ServerSetup::new(&[&id.cert], &id.key).unwrap();

    let (mut io, mut client, mut server, c_step, s_step) =
        setup_streams(1, Faults::default(), &client_setup, &server_setup, NAME);
    assert_eq!(c_step, Step::Done(0));
    assert_eq!(s_step, Step::Done(0));
    assert!(client.is_tls() && server.is_tls());

    for (i, len) in [1u32, 16_384, 16_385, 50_000].into_iter().enumerate() {
        round_trip(&mut io, &mut client, &mut server, len, len, i as u8);
    }
    // Small destination reads trigger in-place spill decryption and drain it cleanly.
    round_trip(&mut io, &mut client, &mut server, 20_000, 100, 99);
    assert!(!client.has_unread_plaintext());
    assert!(client.records() > 0 && server.records() > 0);
}

#[test]
fn tls_streams_survive_short_writes_partial_reads_and_delays() {
    let id = identity(NAME);
    let client_setup = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let server_setup = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let faults = Faults {
        short_send: 300_000,
        partial_recv: 300_000,
        delay: 200_000,
        ..Faults::default()
    };
    for seed in 0..5 {
        let (mut io, mut client, mut server, c_step, s_step) =
            setup_streams(seed, faults, &client_setup, &server_setup, NAME);
        assert_eq!(c_step, Step::Done(0));
        assert_eq!(s_step, Step::Done(0));
        round_trip(
            &mut io,
            &mut client,
            &mut server,
            40_000,
            40_000,
            seed as u8,
        );
    }
}

#[test]
fn tls_alerts_close_notify_idle_check_and_reset() {
    let id = identity(NAME);
    let client_setup = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let server_setup = ServerSetup::new(&[&id.cert], &id.key).unwrap();

    let mut out = Completions::new(8);
    {
        // 1. Idle check skips post-handshake session tickets and sees Clean; fatal alert surfaces Alert(116).
        let (mut io, mut client, mut server, c_step, s_step) =
            setup_streams(11, Faults::default(), &client_setup, &server_setup, NAME);
        assert_eq!((c_step, s_step), (Step::Done(0), Step::Done(0)));

        sealed(|| {
            let mut check = client.check_idle(&mut io, SCRATCH, 0);
            while check == IdleCheck::Pending {
                io.tick(true, &mut out).unwrap();
                let c = out.pop().unwrap();
                assert_eq!(c.token, C_RECV);
                check = client.on_idle_check(&mut io, c.result);
            }
            assert_eq!(check, IdleCheck::Clean);

            server.send_alert(&mut io, S_BUF, 0, 2, 116);
            let mut r = client.recv(&mut io, C_RX, 0, 16);
            while matches!(r, Step::Pending) {
                io.tick(true, &mut out).unwrap();
                while let Some(c) = out.pop() {
                    match c.token {
                        S_SEND => assert!(matches!(
                            server.on_send(&mut io, c.result),
                            Step::Pending | Step::Done(2)
                        )),
                        C_RECV => r = client.on_recv(&mut io, c.result),
                        other => panic!("unexpected token {other:?}"),
                    }
                }
            }
            assert_eq!(r, Step::Tls(TlsError::Alert(116)));
        });

        client.reset();
        assert!(!client.is_tls());
        assert!(!client.has_unread_plaintext());
        assert_eq!(client.records(), 0);
    }
    {
        // 2. close_notify alert (level 1, description 0) ends the stream cleanly with Done(0).
        let (mut io, mut client, mut server, c_step, s_step) =
            setup_streams(12, Faults::default(), &client_setup, &server_setup, NAME);
        assert_eq!((c_step, s_step), (Step::Done(0), Step::Done(0)));
        sealed(|| {
            server.send_alert(&mut io, S_BUF, 0, 1, 0);
            let mut r = client.recv(&mut io, C_RX, 0, 16);
            while matches!(r, Step::Pending) {
                io.tick(true, &mut out).unwrap();
                while let Some(c) = out.pop() {
                    match c.token {
                        S_SEND => assert!(matches!(
                            server.on_send(&mut io, c.result),
                            Step::Pending | Step::Done(2)
                        )),
                        C_RECV => r = client.on_recv(&mut io, c.result),
                        other => panic!("unexpected token {other:?}"),
                    }
                }
            }
            assert_eq!(r, Step::Done(0));
        });
    }
}

#[test]
fn tls_handshake_rejects_untrusted_and_misnamed_certificates() {
    let server_id = identity(NAME);
    let other_id = identity(NAME);
    let client_setup = ClientSetup::with_roots(&[&other_id.cert]).unwrap();
    let server_setup = ServerSetup::new(&[&server_id.cert], &server_id.key).unwrap();
    let (_, _, _, untrusted, _) =
        setup_streams(21, Faults::default(), &client_setup, &server_setup, NAME);
    assert_eq!(untrusted, Step::Tls(TlsError::BadCertificate));

    let right_client = ClientSetup::with_roots(&[&server_id.cert]).unwrap();
    let (_, _, _, misnamed, _) = setup_streams(
        22,
        Faults::default(),
        &right_client,
        &server_setup,
        "wrong.test",
    );
    assert_eq!(misnamed, Step::Tls(TlsError::BadCertificate));
}
