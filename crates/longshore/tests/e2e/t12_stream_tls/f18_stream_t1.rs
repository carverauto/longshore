// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 18 Tier 1 E2E tests: `longshore_stream` plain & TLS stream operations.

#[cfg(feature = "sim")]
use super::super::harness::{drive_until, read_pool_bytes, write_pool_bytes};
#[cfg(all(feature = "sim", feature = "tls"))]
use super::super::harness::{make_tls_identity, TestIdentity};
#[cfg(all(feature = "sim", feature = "tls"))]
use super::stream_rig::drive_tls_handshakes;
#[cfg(feature = "sim")]
use super::stream_rig::{connect_pair_sim, CLIENT, SERVER};
#[cfg(feature = "sim")]
use longshore::io::{BufId, Completions, Io, Limits, Op, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(feature = "sim")]
use longshore::stream::Stream;
#[cfg(feature = "sim")]
use longshore::stream::{IdleCheck, Step};
#[cfg(all(feature = "sim", feature = "tls"))]
use longshore::tls::handshake::Handshake;
use std::process::Command;

/// The client sends `payload` in chunks of up to 4096 bytes and the server
/// echoes each piece back; returns what the client read back.
#[cfg(feature = "sim")]
fn plain_echo(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
    payload: &[u8],
) -> Vec<u8> {
    let (mut c_sent, mut s_recvd, mut s_sent, mut c_recvd) = (0usize, 0usize, 0usize, 0usize);
    let mut echoed = Vec::with_capacity(payload.len());

    let first_chunk = payload.len().min(4096);
    write_pool_bytes(io, BufId(0), 0, &payload[..first_chunk]);
    client.send(io, BufId(0), 0, first_chunk as u32, 2048);
    assert_eq!(server.recv(io, BufId(1), 0, 4096), Step::Pending);
    assert_eq!(client.recv(io, BufId(2), 0, 4096), Step::Pending);

    for _ in 0..500 {
        if c_recvd >= payload.len() {
            break;
        }
        io.tick(true, out).expect("tick");
        while let Some(c) = out.pop() {
            match c.token.0 {
                1 => {
                    if let Step::Done(n) = client.on_send(io, c.result) {
                        assert!(n > 0);
                        c_sent += usize::try_from(n).unwrap();
                        if c_sent < payload.len() {
                            let rem = (payload.len() - c_sent).min(4096);
                            write_pool_bytes(io, BufId(0), 0, &payload[c_sent..c_sent + rem]);
                            client.send(io, BufId(0), 0, rem as u32, 2048);
                        }
                    }
                }
                4 => {
                    if let Step::Done(n) = server.on_recv(io, c.result) {
                        assert!(n > 0);
                        let got = read_pool_bytes(io, BufId(1), 0, usize::try_from(n).unwrap());
                        s_recvd += got.len();
                        write_pool_bytes(io, BufId(3), 0, &got);
                        server.send(io, BufId(3), 0, got.len() as u32, 4096);
                    }
                }
                3 => {
                    if let Step::Done(n) = server.on_send(io, c.result) {
                        assert!(n > 0);
                        s_sent += usize::try_from(n).unwrap();
                        if s_recvd < payload.len() && s_sent == s_recvd {
                            assert_eq!(server.recv(io, BufId(1), 0, 4096), Step::Pending);
                        }
                    }
                }
                2 => {
                    if let Step::Done(n) = client.on_recv(io, c.result) {
                        assert!(n > 0);
                        echoed.extend_from_slice(&read_pool_bytes(
                            io,
                            BufId(2),
                            0,
                            usize::try_from(n).unwrap(),
                        ));
                        c_recvd += usize::try_from(n).unwrap();
                        if c_recvd < payload.len() {
                            assert_eq!(client.recv(io, BufId(2), 0, 4096), Step::Pending);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    echoed
}

#[cfg(feature = "sim")]
#[test]
fn t1_f18_1_plain_stream_bidirectional_echo() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 4,
        buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 1801, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token(1), Token(2));
    let mut server = Stream::new(SERVER, Token(3), Token(4));
    assert!(!client.is_tls() && client.records() == 0);

    let payload: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let echoed = plain_echo(&mut io, &mut out, &mut client, &mut server, &payload);
    assert_eq!(echoed, payload);
}

#[cfg(all(feature = "sim", feature = "tls"))]
#[test]
fn t1_f18_2_tls_stream_client_and_server_handshake_and_echo() {
    let ident: TestIdentity = make_tls_identity(&["stream.test"]);
    let cap = 64 * 1024;
    let limits = Limits {
        sockets_max: 4,
        buffers: 6,
        buffer_size: cap,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 1802, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token(1), Token(2)).with_tls_buffers(
        BufId(0),
        BufId(1),
        cap,
        cap,
        4096,
    );
    let mut server = Stream::new(SERVER, Token(3), Token(4)).with_tls_buffers(
        BufId(2),
        BufId(3),
        cap,
        cap,
        4096,
    );
    let c_hs = Handshake::client(&ident.client_setup(), "stream.test").expect("client hs");
    let s_hs = Handshake::server(&ident.server_setup()).expect("server hs");
    let (cs, ss) = drive_tls_handshakes(&mut io, &mut out, &mut client, &mut server, c_hs, s_hs);
    assert_eq!(cs, Step::Done(0));
    assert_eq!(ss, Step::Done(0));
    assert!(client.is_tls() && server.is_tls());

    let msg = b"hello from opaque-box tls stream";
    write_pool_bytes(&mut io, BufId(4), 0, msg);
    client.send(&mut io, BufId(4), 0, msg.len() as u32, 4096);
    let mut s_step = server.recv(&mut io, BufId(5), 0, cap);
    let mut c_step = Step::Pending;
    for _ in 0..100 {
        if matches!(c_step, Step::Done(_)) && matches!(s_step, Step::Done(_)) {
            break;
        }
        io.tick(true, &mut out).expect("tick");
        while let Some(c) = out.pop() {
            match c.token.0 {
                1 => c_step = client.on_send(&mut io, c.result),
                4 => s_step = server.on_recv(&mut io, c.result),
                _ => {}
            }
        }
    }
    assert_eq!(c_step, Step::Done(i32::try_from(msg.len()).unwrap()));
    assert_eq!(s_step, Step::Done(i32::try_from(msg.len()).unwrap()));
    assert_eq!(read_pool_bytes(&mut io, BufId(5), 0, msg.len()), msg);
    assert!(client.records() > 0 && server.records() > 0);
}

/// Receives `len` bytes on the server while the client's send completes;
/// returns whether the send completed and the bytes received.
#[cfg(all(feature = "sim", feature = "tls"))]
fn recv_big(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
    len: usize,
    cap: u32,
) -> (bool, Vec<u8>) {
    let mut c_done = false;
    let mut recvd = Vec::new();
    let mut s_step = server.recv(io, BufId(5), 0, cap);
    if let Step::Done(n) = s_step {
        recvd.extend_from_slice(&read_pool_bytes(
            io,
            BufId(5),
            0,
            usize::try_from(n).unwrap(),
        ));
    }
    for _ in 0..200 {
        if c_done && recvd.len() >= len {
            break;
        }
        io.tick(true, out).expect("tick");
        while let Some(c) = out.pop() {
            match c.token.0 {
                1 => {
                    if matches!(client.on_send(io, c.result), Step::Done(_)) {
                        c_done = true;
                    }
                }
                4 => {
                    s_step = server.on_recv(io, c.result);
                    if let Step::Done(n) = s_step {
                        assert!(n > 0);
                        recvd.extend_from_slice(&read_pool_bytes(
                            io,
                            BufId(5),
                            0,
                            usize::try_from(n).unwrap(),
                        ));
                        if recvd.len() < len {
                            let _: Step = server.recv(io, BufId(5), 0, cap);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    (c_done, recvd)
}

#[cfg(all(feature = "sim", feature = "tls"))]
#[test]
fn t1_f18_3_tls_stream_batch_clamping_and_zero_copy_threshold() {
    let ident = make_tls_identity(&["batch.test"]);
    let cap = 64 * 1024;
    let limits = Limits {
        sockets_max: 4,
        buffers: 6,
        buffer_size: cap,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 1803, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client =
        Stream::new(CLIENT, Token(1), Token(2)).with_tls_buffers(BufId(0), BufId(1), cap, 1, 4096);
    let mut server = Stream::new(SERVER, Token(3), Token(4)).with_tls_buffers(
        BufId(2),
        BufId(3),
        cap,
        u32::MAX,
        4096,
    );
    let c_hs = Handshake::client(&ident.client_setup(), "batch.test").unwrap();
    let s_hs = Handshake::server(&ident.server_setup()).unwrap();
    assert_eq!(
        drive_tls_handshakes(&mut io, &mut out, &mut client, &mut server, c_hs, s_hs),
        (Step::Done(0), Step::Done(0))
    );

    let big: Vec<u8> = (0..(20 * 1024u32)).map(|i| (i * 7) as u8).collect();
    write_pool_bytes(&mut io, BufId(4), 0, &big);
    client.send(&mut io, BufId(4), 0, big.len() as u32, 4096);
    let (c_done, recvd) = recv_big(&mut io, &mut out, &mut client, &mut server, big.len(), cap);
    assert!(c_done);
    assert_eq!(recvd, big);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f18_4_stream_idle_check_clean_dirty_and_closed() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 1804, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token(1), Token(2));
    let mut server = Stream::new(SERVER, Token(3), Token(4));

    assert_eq!(client.check_idle(&mut io, BufId(0), 0), IdleCheck::Pending);
    io.tick(true, &mut out).expect("tick");
    let c = out.pop().expect("idle completion");
    assert_eq!(client.on_idle_check(&mut io, c.result), IdleCheck::Clean);

    write_pool_bytes(&mut io, BufId(1), 0, b"X");
    server.send(&mut io, BufId(1), 0, 1, 4096);
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 20, |v| !v.is_empty()).unwrap();
    assert_eq!(client.check_idle(&mut io, BufId(0), 0), IdleCheck::Pending);
    io.tick(true, &mut out).expect("tick");
    let dirty = out.pop().expect("dirty completion");
    assert_eq!(
        client.on_idle_check(&mut io, dirty.result),
        IdleCheck::Dirty
    );

    io.submit(Token(99), Op::Close { sock: SERVER }).unwrap();
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 20, |v| !v.is_empty()).unwrap();
    assert_eq!(client.check_idle(&mut io, BufId(0), 0), IdleCheck::Pending);
    io.tick(true, &mut out).expect("tick");
    let closed = out.pop().expect("closed completion");
    assert_eq!(
        client.on_idle_check(&mut io, closed.result),
        IdleCheck::Closed
    );
}

#[test]
fn t1_f18_5_longshore_stream_has_zero_http_dependency() {
    let root = crate::e2e::harness::workspace_root();
    let out = Command::new("cargo")
        .args(["tree", "-p", "longshore_stream", "-e", "normal"])
        .current_dir(root)
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("longshore_http_client"),
        "longshore_stream must not depend on longshore_http_client:\n{stdout}"
    );
}
