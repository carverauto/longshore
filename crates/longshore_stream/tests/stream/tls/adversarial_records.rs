// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial tests for TLS `Stream`: `IdleCheck` edges, forged and
//! tampered records, `on_handshake` reset/closure, and `Stream::reset` reuse.

use super::{
    identity, setup_streams, BUF, CLIENT, C_CONN, C_RECV, C_SEND, C_TLS_RX, C_TLS_TX, C_TX,
    LISTENER, NAME, SCRATCH, SERVER, S_ACCEPT, S_BUF, S_RECV, S_SEND, S_TLS_RX, S_TLS_TX,
};
use longshore_common::io::{
    Completion, Completions, Io, Limits, Op, Token, EAGAIN, ECONNRESET, EPIPE,
};
use longshore_common::sim::{Faults, SimIo};
use longshore_stream::{IdleCheck, Step, Stream, TlsError};
use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[test]
fn tls_stream_idle_edges_corrupt_records_handshake_errors_and_reset_reuse() {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let mut out = Completions::new(16);

    unsolicited_data_is_dirty(&cs, &ss, &mut out);
    partial_record_then_eagain_is_closed(&cs, &ss, &mut out);
    close_notify_is_closed(&cs, &ss, &mut out);
    bad_header_and_tampered_record_are_refused(&cs, &ss, &mut out);
    handshake_errors_then_reset_and_reuse(&cs, &ss, &mut out);
}

/// 1. Unsolicited app data -> IdleCheck::Dirty.
fn unsolicited_data_is_dirty(cs: &ClientSetup, ss: &ServerSetup, out: &mut Completions) {
    let (mut io, mut client, mut server, _, _) = setup_streams(45, Faults::default(), cs, ss, NAME);
    io.buf(C_TX)[..4].copy_from_slice(b"push");
    client.send(&mut io, C_TX, 0, 4, u32::MAX);
    flush_client_send(&mut io, out, &mut client);
    assert_eq!(server.check_idle(&mut io, SCRATCH, 0), IdleCheck::Pending);
    io.tick(true, out).unwrap();
    let idle_check = out.pop().unwrap();
    assert_eq!(
        server.on_idle_check(&mut io, idle_check.result),
        IdleCheck::Dirty
    );
}

/// 2. Partial record on -EAGAIN -> IdleCheck::Closed.
fn partial_record_then_eagain_is_closed(cs: &ClientSetup, ss: &ServerSetup, out: &mut Completions) {
    let (mut io2, _client2, mut server2, _, _) = setup_streams(46, Faults::default(), cs, ss, NAME);
    raw_send(&mut io2, out, 999, &[0x17, 0x03, 0x03]);
    assert_eq!(server2.check_idle(&mut io2, SCRATCH, 0), IdleCheck::Pending);
    io2.tick(true, out).unwrap();
    let c2 = out.pop().unwrap();
    assert_eq!(
        server2.on_idle_check(&mut io2, c2.result),
        IdleCheck::Pending
    );
    io2.tick(false, out).unwrap();
    let c_again = out.pop().unwrap();
    assert_eq!(c_again.result, -EAGAIN);
    assert_eq!(
        server2.on_idle_check(&mut io2, c_again.result),
        IdleCheck::Closed
    );
}

/// 3. Unsolicited close_notify -> IdleCheck::Closed; and recv close_notify -> self.closed -> Closed.
fn close_notify_is_closed(cs: &ClientSetup, ss: &ServerSetup, out: &mut Completions) {
    let (mut io_c, mut client_c, mut server_c, _, _) =
        setup_streams(44, Faults::default(), cs, ss, NAME);
    client_c.send_alert(&mut io_c, C_TX, 0, 1, 0);
    flush_client_send(&mut io_c, out, &mut client_c);
    assert_eq!(
        server_c.check_idle(&mut io_c, SCRATCH, 0),
        IdleCheck::Pending
    );
    io_c.tick(true, out).unwrap();
    let c3 = out.pop().unwrap();
    assert_eq!(
        server_c.on_idle_check(&mut io_c, c3.result),
        IdleCheck::Closed
    );

    let (mut io_c2, mut client_c2, mut server_c2, _, _) =
        setup_streams(43, Faults::default(), cs, ss, NAME);
    client_c2.send_alert(&mut io_c2, C_TX, 0, 1, 0);
    flush_client_send(&mut io_c2, out, &mut client_c2);
    assert_eq!(server_c2.recv(&mut io_c2, S_BUF, 0, 16), Step::Pending);
    io_c2.tick(true, out).unwrap();
    let c3b = out.pop().unwrap();
    assert_eq!(server_c2.on_recv(&mut io_c2, c3b.result), Step::Done(0));
    assert_eq!(
        server_c2.check_idle(&mut io_c2, SCRATCH, 0),
        IdleCheck::Closed
    );
}

/// 4. Bad record header -> TlsError::BadRecord; tampered AEAD record -> TlsError::Record.
fn bad_header_and_tampered_record_are_refused(
    cs: &ClientSetup,
    ss: &ServerSetup,
    out: &mut Completions,
) {
    let (mut io3, _, mut server3, _, _) = setup_streams(47, Faults::default(), cs, ss, NAME);
    raw_send(&mut io3, out, 888, &[0x14, 0x03, 0x03, 0x00, 0x01, 0x01]);
    assert_eq!(server3.recv(&mut io3, S_BUF, 0, 16), Step::Pending);
    io3.tick(true, out).unwrap();
    let c4 = out.pop().unwrap();
    assert_eq!(
        server3.on_recv(&mut io3, c4.result),
        Step::Tls(TlsError::BadRecord)
    );

    let (mut io4, _, mut server4, _, _) = setup_streams(48, Faults::default(), cs, ss, NAME);
    let mut forged = [0u8; 26];
    forged[..5].copy_from_slice(&[0x17, 0x03, 0x03, 0x00, 21]);
    raw_send(&mut io4, out, 889, &forged);
    assert_eq!(server4.recv(&mut io4, S_BUF, 0, 16), Step::Pending);
    io4.tick(true, out).unwrap();
    let c5 = out.pop().unwrap();
    assert_eq!(
        server4.on_recv(&mut io4, c5.result),
        Step::Tls(TlsError::Record)
    );
}

/// 5. on_handshake(0) -> -ECONNRESET; on_handshake(<0) -> result; reset() + second handshake works.
fn handshake_errors_then_reset_and_reuse(
    cs: &ClientSetup,
    ss: &ServerSetup,
    out: &mut Completions,
) {
    let mut io5 = SimIo::new(
        Limits {
            sockets_max: 4,
            buffers: 8,
            buffer_size: BUF,
            ops_max: 8,
            ..Limits::BASE
        },
        49,
        Faults::default(),
    );
    let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 21)), 443);
    let _: SocketAddr = io5.listen(LISTENER, addr2).unwrap();
    connect_both(&mut io5, out, addr2);
    let mut s_reuse =
        Stream::new(CLIENT, C_SEND, C_RECV).with_tls_buffers(C_TLS_TX, C_TLS_RX, BUF, BUF, 4096);
    let mut srv_reuse =
        Stream::new(SERVER, S_SEND, S_RECV).with_tls_buffers(S_TLS_TX, S_TLS_RX, BUF, BUF, 4096);
    let _: Step = s_reuse.start_handshake(&mut io5, Handshake::client(cs, NAME).unwrap());
    assert_eq!(
        s_reuse.on_handshake(&mut io5, false, 0),
        Step::Done(-ECONNRESET)
    );
    assert_eq!(
        s_reuse.on_handshake(&mut io5, true, -EPIPE),
        Step::Done(-EPIPE)
    );

    // Reset sockets + Streams and run a clean second handshake on the same Stream structs.
    io5.submit(Token(501), Op::Close { sock: CLIENT }).unwrap();
    io5.submit(Token(502), Op::Close { sock: SERVER }).unwrap();
    tick_until_completions(&mut io5, out, 2);
    connect_both(&mut io5, out, addr2);
    s_reuse.reset();
    srv_reuse.reset();
    assert!(!s_reuse.is_tls());
    let steps = handshake(&mut io5, out, cs, ss, &mut s_reuse, &mut srv_reuse);
    assert_eq!(steps, (Step::Done(0), Step::Done(0)));
}

/// Accepts on `LISTENER` into `SERVER` and connects `CLIENT` to `addr`,
/// ticking until both are up.
fn connect_both(io: &mut SimIo, out: &mut Completions, addr: SocketAddr) {
    io.submit(
        S_ACCEPT,
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )
    .unwrap();
    io.submit(C_CONN, Op::Connect { sock: CLIENT, addr })
        .unwrap();
    tick_until_completions(io, out, 2);
}

/// Ticks until `n` completions of any kind have arrived.
fn tick_until_completions(io: &mut SimIo, out: &mut Completions, n: usize) {
    let mut seen = 0;
    while seen < n {
        io.tick(true, out).unwrap();
        while out.pop().is_some() {
            seen += 1;
        }
    }
}

/// Runs a full handshake between `client` and `server`; returns the final steps.
fn handshake(
    io: &mut SimIo,
    out: &mut Completions,
    cs: &ClientSetup,
    ss: &ServerSetup,
    client: &mut Stream,
    server: &mut Stream,
) -> (Step, Step) {
    let mut sc = client.start_handshake(io, Handshake::client(cs, NAME).unwrap());
    let mut ss2 = server.start_handshake(io, Handshake::server(ss).unwrap());
    while matches!(sc, Step::Pending) || matches!(ss2, Step::Pending) {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            match c.token {
                C_SEND => sc = client.on_handshake(io, true, c.result),
                C_RECV => sc = client.on_handshake(io, false, c.result),
                S_SEND => ss2 = server.on_handshake(io, true, c.result),
                S_RECV => ss2 = server.on_handshake(io, false, c.result),
                _ => {}
            }
        }
    }
    (sc, ss2)
}

/// Ticks once and hands the client its send completions.
fn flush_client_send(io: &mut SimIo, out: &mut Completions, client: &mut Stream) {
    io.tick(true, out).unwrap();
    while let Some(c) = out.pop() {
        if c.token == C_SEND {
            let _: Step = client.on_send(io, c.result);
        }
    }
}

fn raw_send(io: &mut SimIo, out: &mut Completions, tok: u64, bytes: &[u8]) {
    io.buf(C_TX)[..bytes.len()].copy_from_slice(bytes);
    let op = Op::Send {
        sock: CLIENT,
        buf: C_TX,
        off: 0,
        len: bytes.len() as u32,
        zero_copy: false,
    };
    io.submit(Token(tok), op).unwrap();
    io.tick(true, out).unwrap();
    let _: Option<Completion> = out.pop();
}
