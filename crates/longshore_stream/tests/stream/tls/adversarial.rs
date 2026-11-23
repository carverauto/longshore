// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial tests for TLS `Stream`: `borrow`, `make_room` compaction,
//! zero-length & error paths, forged/tampered records, `IdleCheck` edges,
//! `on_handshake` reset/closure, and `Stream::reset` reuse.

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
fn tls_stream_borrow_fallback_make_room_compaction_and_zero_send() {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let (mut io, mut client, mut server, c_step, s_step) =
        setup_streams(31, Faults::default(), &cs, &ss, NAME);
    assert_eq!((c_step, s_step), (Step::Done(0), Step::Done(0)));

    let mut out = Completions::new(16);

    // 1. Borrow-1-byte path: dst_max = 10, sentinel at dst[10] preserved.
    io.buf(C_TX)[..10].copy_from_slice(b"0123456789");
    client.send(&mut io, C_TX, 0, 10, u32::MAX);
    while io.tick(true, &mut out).is_ok() {
        let mut sent = false;
        while let Some(c) = out.pop() {
            if c.token == C_SEND && matches!(client.on_send(&mut io, c.result), Step::Done(10)) {
                sent = true;
            }
        }
        if sent {
            break;
        }
    }
    io.buf(S_BUF).fill(0xCC);
    let mut step = server.recv(&mut io, S_BUF, 0, 10);
    while matches!(step, Step::Pending) {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == S_RECV {
                step = server.on_recv(&mut io, c.result);
            }
        }
    }
    assert_eq!(step, Step::Done(10));
    assert_eq!(&io.buf(S_BUF)[..10], b"0123456789");
    assert_eq!(
        io.buf(S_BUF)[10],
        0xCC,
        "borrow path must restore trailing byte"
    );

    // 2. Fallback when at + space == dst.len() (no room for 1-byte borrow).
    io.buf(C_TX)[..10].copy_from_slice(b"abcdefghij");
    client.send(&mut io, C_TX, 0, 10, u32::MAX);
    while io.tick(true, &mut out).is_ok() {
        let mut sent = false;
        while let Some(c) = out.pop() {
            if c.token == C_SEND && matches!(client.on_send(&mut io, c.result), Step::Done(10)) {
                sent = true;
            }
        }
        if sent {
            break;
        }
    }
    let end_off = BUF - 10;
    let mut step2 = server.recv(&mut io, S_BUF, end_off, 10);
    while matches!(step2, Step::Pending) {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == S_RECV {
                step2 = server.on_recv(&mut io, c.result);
            }
        }
    }
    assert_eq!(step2, Step::Done(10));
    assert_eq!(&io.buf(S_BUF)[end_off as usize..], b"abcdefghij");

    // 3. Zero-length TLS send completes Done(0); negative on_send returns Done(err).
    client.send(&mut io, C_TX, 0, 0, u32::MAX);
    io.tick(true, &mut out).unwrap();
    let empty_send = out.pop().unwrap();
    assert_eq!(client.on_send(&mut io, empty_send.result), Step::Done(0));
    assert_eq!(client.on_send(&mut io, -EPIPE), Step::Done(-EPIPE));

    // 4. Spill triggers IdleCheck::Dirty; dst_max == 0 preserves spill; draining restores Clean.
    io.buf(C_TX)[..20].copy_from_slice(b"ABCDEFGHIJKLMNOPQRST");
    client.send(&mut io, C_TX, 0, 20, u32::MAX);
    io.tick(true, &mut out).unwrap();
    while let Some(c) = out.pop() {
        if c.token == C_SEND {
            let _: Step = client.on_send(&mut io, c.result);
        }
    }
    let mut r = server.recv(&mut io, S_BUF, 0, 5);
    while matches!(r, Step::Pending) {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == S_RECV {
                r = server.on_recv(&mut io, c.result);
            }
        }
    }
    assert_eq!(r, Step::Done(5));
    assert!(server.has_unread_plaintext());
    assert_eq!(server.recv(&mut io, S_BUF, 0, 0), Step::Done(0));
    assert_eq!(server.check_idle(&mut io, SCRATCH, 0), IdleCheck::Dirty);
    assert_eq!(server.recv(&mut io, S_BUF, 0, 15), Step::Done(15));
    assert!(!server.has_unread_plaintext());

    // 5. Fill 64 KiB rx_buf to cap while rx_start > 0 so make_room compacts via copy_within.
    let big: Vec<u8> = (0..BUF as usize).map(|i| (i * 31 + 7) as u8).collect();
    io.buf(C_TX)[..big.len()].copy_from_slice(&big);
    client.send(&mut io, C_TX, 0, big.len() as u32, u32::MAX);
    let (mut queued, mut client_done) = (0i32, false);
    while queued < i32::try_from(BUF).unwrap() {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == C_SEND {
                queued += c.result;
                assert!(matches!(client.on_send(&mut io, c.result), Step::Pending));
            }
        }
    }
    assert_eq!(queued, i32::try_from(BUF).unwrap());
    let (mut got, mut rx_reads) = (Vec::with_capacity(big.len()), Vec::new());
    while got.len() < big.len() {
        let mut s = server.recv(&mut io, S_BUF, 0, 16_384);
        while matches!(s, Step::Pending) {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                match c.token {
                    C_SEND => {
                        client_done = client.on_send(&mut io, c.result)
                            == Step::Done(i32::try_from(BUF).unwrap())
                    }
                    S_RECV => {
                        rx_reads.push(c.result);
                        s = server.on_recv(&mut io, c.result);
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(s, Step::Done(16_384));
        got.extend_from_slice(&io.buf(S_BUF)[..16_384]);
    }
    assert!(client_done);
    assert_eq!(rx_reads, vec![i32::try_from(BUF).unwrap(), 88]);
    assert_eq!(got, big);
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

#[test]
fn tls_stream_idle_edges_corrupt_records_handshake_errors_and_reset_reuse() {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let (mut io, mut client, mut server, _, _) =
        setup_streams(45, Faults::default(), &cs, &ss, NAME);
    let mut out = Completions::new(16);

    // 1. Unsolicited app data -> IdleCheck::Dirty.
    io.buf(C_TX)[..4].copy_from_slice(b"push");
    client.send(&mut io, C_TX, 0, 4, u32::MAX);
    io.tick(true, &mut out).unwrap();
    while let Some(c) = out.pop() {
        if c.token == C_SEND {
            let _: Step = client.on_send(&mut io, c.result);
        }
    }
    assert_eq!(server.check_idle(&mut io, SCRATCH, 0), IdleCheck::Pending);
    io.tick(true, &mut out).unwrap();
    let idle_check = out.pop().unwrap();
    assert_eq!(
        server.on_idle_check(&mut io, idle_check.result),
        IdleCheck::Dirty
    );

    // 2. Partial record on -EAGAIN -> IdleCheck::Closed.
    let (mut io2, _client2, mut server2, _, _) =
        setup_streams(46, Faults::default(), &cs, &ss, NAME);
    raw_send(&mut io2, &mut out, 999, &[0x17, 0x03, 0x03]);
    assert_eq!(server2.check_idle(&mut io2, SCRATCH, 0), IdleCheck::Pending);
    io2.tick(true, &mut out).unwrap();
    let c2 = out.pop().unwrap();
    assert_eq!(
        server2.on_idle_check(&mut io2, c2.result),
        IdleCheck::Pending
    );
    io2.tick(false, &mut out).unwrap();
    let c_again = out.pop().unwrap();
    assert_eq!(c_again.result, -EAGAIN);
    assert_eq!(
        server2.on_idle_check(&mut io2, c_again.result),
        IdleCheck::Closed
    );

    // 3. Unsolicited close_notify -> IdleCheck::Closed; and recv close_notify -> self.closed -> Closed.
    let (mut io_c, mut client_c, mut server_c, _, _) =
        setup_streams(44, Faults::default(), &cs, &ss, NAME);
    client_c.send_alert(&mut io_c, C_TX, 0, 1, 0);
    io_c.tick(true, &mut out).unwrap();
    while let Some(c) = out.pop() {
        if c.token == C_SEND {
            let _: Step = client_c.on_send(&mut io_c, c.result);
        }
    }
    assert_eq!(
        server_c.check_idle(&mut io_c, SCRATCH, 0),
        IdleCheck::Pending
    );
    io_c.tick(true, &mut out).unwrap();
    let c3 = out.pop().unwrap();
    assert_eq!(
        server_c.on_idle_check(&mut io_c, c3.result),
        IdleCheck::Closed
    );

    let (mut io_c2, mut client_c2, mut server_c2, _, _) =
        setup_streams(43, Faults::default(), &cs, &ss, NAME);
    client_c2.send_alert(&mut io_c2, C_TX, 0, 1, 0);
    io_c2.tick(true, &mut out).unwrap();
    while let Some(c) = out.pop() {
        if c.token == C_SEND {
            let _: Step = client_c2.on_send(&mut io_c2, c.result);
        }
    }
    assert_eq!(server_c2.recv(&mut io_c2, S_BUF, 0, 16), Step::Pending);
    io_c2.tick(true, &mut out).unwrap();
    let c3b = out.pop().unwrap();
    assert_eq!(server_c2.on_recv(&mut io_c2, c3b.result), Step::Done(0));
    assert_eq!(
        server_c2.check_idle(&mut io_c2, SCRATCH, 0),
        IdleCheck::Closed
    );

    // 4. Bad record header -> TlsError::BadRecord; tampered AEAD record -> TlsError::Record.
    let (mut io3, _, mut server3, _, _) = setup_streams(47, Faults::default(), &cs, &ss, NAME);
    raw_send(
        &mut io3,
        &mut out,
        888,
        &[0x14, 0x03, 0x03, 0x00, 0x01, 0x01],
    );
    assert_eq!(server3.recv(&mut io3, S_BUF, 0, 16), Step::Pending);
    io3.tick(true, &mut out).unwrap();
    let c4 = out.pop().unwrap();
    assert_eq!(
        server3.on_recv(&mut io3, c4.result),
        Step::Tls(TlsError::BadRecord)
    );

    let (mut io4, _, mut server4, _, _) = setup_streams(48, Faults::default(), &cs, &ss, NAME);
    let mut forged = [0u8; 26];
    forged[..5].copy_from_slice(&[0x17, 0x03, 0x03, 0x00, 21]);
    raw_send(&mut io4, &mut out, 889, &forged);
    assert_eq!(server4.recv(&mut io4, S_BUF, 0, 16), Step::Pending);
    io4.tick(true, &mut out).unwrap();
    let c5 = out.pop().unwrap();
    assert_eq!(
        server4.on_recv(&mut io4, c5.result),
        Step::Tls(TlsError::Record)
    );

    // 5. on_handshake(0) -> -ECONNRESET; on_handshake(<0) -> result; reset() + second handshake works.
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
    io5.submit(
        S_ACCEPT,
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )
    .unwrap();
    io5.submit(
        C_CONN,
        Op::Connect {
            sock: CLIENT,
            addr: addr2,
        },
    )
    .unwrap();
    let mut up = 0;
    while up < 2 {
        io5.tick(true, &mut out).unwrap();
        while out.pop().is_some() {
            up += 1;
        }
    }
    let mut s_reuse =
        Stream::new(CLIENT, C_SEND, C_RECV).with_tls_buffers(C_TLS_TX, C_TLS_RX, BUF, BUF, 4096);
    let mut srv_reuse =
        Stream::new(SERVER, S_SEND, S_RECV).with_tls_buffers(S_TLS_TX, S_TLS_RX, BUF, BUF, 4096);
    let _: Step = s_reuse.start_handshake(&mut io5, Handshake::client(&cs, NAME).unwrap());
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
    let mut closed_n = 0;
    while closed_n < 2 {
        io5.tick(true, &mut out).unwrap();
        while out.pop().is_some() {
            closed_n += 1;
        }
    }
    io5.submit(
        S_ACCEPT,
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )
    .unwrap();
    io5.submit(
        C_CONN,
        Op::Connect {
            sock: CLIENT,
            addr: addr2,
        },
    )
    .unwrap();
    let mut up2 = 0;
    while up2 < 2 {
        io5.tick(true, &mut out).unwrap();
        while out.pop().is_some() {
            up2 += 1;
        }
    }
    s_reuse.reset();
    srv_reuse.reset();
    assert!(!s_reuse.is_tls());
    let mut sc = s_reuse.start_handshake(&mut io5, Handshake::client(&cs, NAME).unwrap());
    let mut ss2 = srv_reuse.start_handshake(&mut io5, Handshake::server(&ss).unwrap());
    while matches!(sc, Step::Pending) || matches!(ss2, Step::Pending) {
        io5.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            match c.token {
                C_SEND => sc = s_reuse.on_handshake(&mut io5, true, c.result),
                C_RECV => sc = s_reuse.on_handshake(&mut io5, false, c.result),
                S_SEND => ss2 = srv_reuse.on_handshake(&mut io5, true, c.result),
                S_RECV => ss2 = srv_reuse.on_handshake(&mut io5, false, c.result),
                _ => {}
            }
        }
    }
    assert_eq!((sc, ss2), (Step::Done(0), Step::Done(0)));
}
