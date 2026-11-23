// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial tests for TLS `Stream`: `borrow`, `make_room` compaction,
//! zero-length & error paths, and a spill seen by `IdleCheck`. Forged and
//! tampered records, `on_handshake` errors and `Stream::reset` reuse are in
//! `adversarial_records.rs`.

use super::{identity, setup_streams, BUF, C_SEND, C_TX, NAME, SCRATCH, S_BUF, S_RECV};
use longshore_common::io::{Completions, Io, EPIPE};
use longshore_common::sim::{Faults, SimIo};
use longshore_stream::{IdleCheck, Step, Stream};
use longshore_tls::handshake::{ClientSetup, ServerSetup};

#[test]
fn tls_stream_borrow_fallback_make_room_compaction_and_zero_send() {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let (mut io, mut client, mut server, c_step, s_step) =
        setup_streams(31, Faults::default(), &cs, &ss, NAME);
    assert_eq!((c_step, s_step), (Step::Done(0), Step::Done(0)));

    let mut out = Completions::new(16);
    borrow_one_byte_keeps_trailing_byte(&mut io, &mut out, &mut client, &mut server);
    no_room_to_borrow_falls_back(&mut io, &mut out, &mut client, &mut server);
    zero_length_and_failed_sends(&mut io, &mut out, &mut client);
    spill_is_dirty_until_drained(&mut io, &mut out, &mut client, &mut server);
    full_rx_buffer_compacts(&mut io, &mut out, &mut client, &mut server);
}

/// 1. Borrow-1-byte path: dst_max = 10, sentinel at dst[10] preserved.
fn borrow_one_byte_keeps_trailing_byte(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
) {
    send_until_done(io, out, client, b"0123456789");
    io.buf(S_BUF).fill(0xCC);
    assert_eq!(recv_until_done(io, out, server, 0, 10), Step::Done(10));
    assert_eq!(&io.buf(S_BUF)[..10], b"0123456789");
    assert_eq!(
        io.buf(S_BUF)[10],
        0xCC,
        "borrow path must restore trailing byte"
    );
}

/// 2. Fallback when at + space == dst.len() (no room for 1-byte borrow).
fn no_room_to_borrow_falls_back(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
) {
    send_until_done(io, out, client, b"abcdefghij");
    let end_off = BUF - 10;
    assert_eq!(
        recv_until_done(io, out, server, end_off, 10),
        Step::Done(10)
    );
    assert_eq!(&io.buf(S_BUF)[end_off as usize..], b"abcdefghij");
}

/// 3. Zero-length TLS send completes Done(0); negative on_send returns Done(err).
fn zero_length_and_failed_sends(io: &mut SimIo, out: &mut Completions, client: &mut Stream) {
    client.send(io, C_TX, 0, 0, u32::MAX);
    io.tick(true, out).unwrap();
    let empty_send = out.pop().unwrap();
    assert_eq!(client.on_send(io, empty_send.result()), Step::Done(0));
    assert_eq!(client.on_send(io, -EPIPE), Step::Done(-EPIPE));
}

/// 4. Spill triggers IdleCheck::Dirty; dst_max == 0 preserves spill; draining restores Clean.
fn spill_is_dirty_until_drained(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
) {
    io.buf(C_TX)[..20].copy_from_slice(b"ABCDEFGHIJKLMNOPQRST");
    client.send(io, C_TX, 0, 20, u32::MAX);
    io.tick(true, out).unwrap();
    while let Some(c) = out.pop() {
        if c.token() == C_SEND {
            let _: Step = client.on_send(io, c.result());
        }
    }
    assert_eq!(recv_until_done(io, out, server, 0, 5), Step::Done(5));
    assert!(server.has_unread_plaintext());
    assert_eq!(server.recv(io, S_BUF, 0, 0), Step::Done(0));
    assert_eq!(server.check_idle(io, SCRATCH, 0), IdleCheck::Dirty);
    assert_eq!(server.recv(io, S_BUF, 0, 15), Step::Done(15));
    assert!(!server.has_unread_plaintext());
}

/// 5. Fill 64 KiB rx_buf to cap while rx_start > 0 so make_room compacts via copy_within.
fn full_rx_buffer_compacts(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
) {
    let big: Vec<u8> = (0..BUF as usize).map(|i| (i * 31 + 7) as u8).collect();
    io.buf(C_TX)[..big.len()].copy_from_slice(&big);
    client.send(io, C_TX, 0, big.len() as u32, u32::MAX);
    let mut queued = 0i32;
    while queued < i32::try_from(BUF).unwrap() {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() == C_SEND {
                queued += c.result();
                assert!(matches!(client.on_send(io, c.result()), Step::Pending));
            }
        }
    }
    assert_eq!(queued, i32::try_from(BUF).unwrap());
    let (mut got, mut rx_reads) = (Vec::with_capacity(big.len()), Vec::new());
    let mut client_done = false;
    while got.len() < big.len() {
        let step =
            recv_16k_finishing_send(io, out, client, server, &mut client_done, &mut rx_reads);
        assert_eq!(step, Step::Done(16_384));
        got.extend_from_slice(&io.buf(S_BUF)[..16_384]);
    }
    assert!(client_done);
    assert_eq!(rx_reads, vec![i32::try_from(BUF).unwrap(), 88]);
    assert_eq!(got, big);
}

/// One 16 KiB server read, while the client's 64 KiB send finishes. Records
/// each raw read's result in `rx_reads`.
fn recv_16k_finishing_send(
    io: &mut SimIo,
    out: &mut Completions,
    client: &mut Stream,
    server: &mut Stream,
    client_done: &mut bool,
    rx_reads: &mut Vec<i32>,
) -> Step {
    let mut s = server.recv(io, S_BUF, 0, 16_384);
    while matches!(s, Step::Pending) {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            match c.token() {
                C_SEND => {
                    *client_done =
                        client.on_send(io, c.result()) == Step::Done(i32::try_from(BUF).unwrap())
                }
                S_RECV => {
                    rx_reads.push(c.result());
                    s = server.on_recv(io, c.result());
                }
                _ => {}
            }
        }
    }
    s
}

/// Sends `bytes` from the start of the client's buffer and ticks until the
/// client reports all of them sent.
fn send_until_done(io: &mut SimIo, out: &mut Completions, client: &mut Stream, bytes: &[u8]) {
    io.buf(C_TX)[..bytes.len()].copy_from_slice(bytes);
    let len = bytes.len() as u32;
    client.send(io, C_TX, 0, len, u32::MAX);
    let done = Step::Done(i32::try_from(len).unwrap());
    while io.tick(true, out).is_ok() {
        let mut sent = false;
        while let Some(c) = out.pop() {
            if c.token() == C_SEND && client.on_send(io, c.result()) == done {
                sent = true;
            }
        }
        if sent {
            break;
        }
    }
}

/// Asks the server for `len` bytes at `off` and ticks until the read is done.
fn recv_until_done(
    io: &mut SimIo,
    out: &mut Completions,
    server: &mut Stream,
    off: u32,
    len: u32,
) -> Step {
    let mut step = server.recv(io, S_BUF, off, len);
    while matches!(step, Step::Pending) {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token() == S_RECV {
                step = server.on_recv(io, c.result());
            }
        }
    }
    step
}
