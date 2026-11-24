// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulator tests for receiving through buffer-ring buffers: what an armed
//! receive delivers, opened by `Stream::on_ring_recv`, is what was sent.
#![cfg(feature = "tls-core")]

use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_stream::{RingBytes, RingDst, Step, Stream, TlsError, RING_DST_MIN};
use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)), 443);
const NAME: &str = "stream.test";
const BUF: u32 = 256 * 1024;

const LISTENER: Sock = Sock::new(0);
const CLIENT: Sock = Sock::new(1);
const SERVER: Sock = Sock::new(2);

const C_CONN: Token = Token::new(1);
const C_SEND: Token = Token::new(2);
const C_RECV: Token = Token::new(3);
const S_ACCEPT: Token = Token::new(4);
const S_SEND: Token = Token::new(5);
const S_RECV: Token = Token::new(6);

const C_TLS_TX: BufId = BufId::new(0);
const C_TLS_RX: BufId = BufId::new(1);
const S_TLS_TX: BufId = BufId::new(2);
const S_TLS_RX: BufId = BufId::new(3);
const C_TX: BufId = BufId::new(4);
const S_DST: BufId = BufId::new(5);

/// The destination is handed over in pieces of this size, so records are
/// opened at every position in it and the "no room" path is taken.
const DST_PIECE: u32 = RING_DST_MIN + 1000;
/// What is left of a destination that is nearly full: no record fits.
const TIGHT: u32 = 64;

#[cfg(test)]
fn limits(ring_buffer_size: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(3)
        .with_buffers(6)
        .with_buffer_size(BUF)
        .with_ring_buffers(4)
        .with_ring_buffer_size(ring_buffer_size)
        .with_ops_max(8)
}

#[cfg(test)]
struct Pair {
    io: SimIo,
    client: Stream,
    server: Stream,
}

#[cfg(test)]
/// A connected pair; with `tls`, past the handshake.
fn pair(seed: u64, faults: Faults, ring_buffer_size: u32, tls: bool) -> Pair {
    let mut io = SimIo::new(limits(ring_buffer_size), seed, faults);
    let _: SocketAddr = io.listen(LISTENER, ADDR).unwrap();
    let accept = Op::Accept {
        listener: LISTENER,
        sock: SERVER,
    };
    io.submit(S_ACCEPT, accept).unwrap();
    let connect = Op::Connect {
        sock: CLIENT,
        addr: ADDR,
    };
    io.submit(C_CONN, connect).unwrap();
    let mut out = Completions::new(8);
    let mut up = 0;
    while up < 2 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            assert!(c.result() >= 0);
            up += 1;
        }
    }
    let mut client = Stream::new(CLIENT, C_SEND, C_RECV);
    let mut server = Stream::new(SERVER, S_SEND, S_RECV);
    if tls {
        let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
        let (cert, key) = (k.cert.der().to_vec(), k.signing_key.serialize_der());
        let cs = ClientSetup::with_roots(&[&cert]).unwrap();
        let ss = ServerSetup::new(&[&cert], &key).unwrap();
        client = client.with_tls_buffers(C_TLS_TX, C_TLS_RX, BUF, BUF, 4096);
        server = server.with_tls_buffers(S_TLS_TX, S_TLS_RX, BUF, BUF, 4096);
        let mut c = client.start_handshake(&mut io, Handshake::client(&cs, NAME).unwrap());
        let mut s = server.start_handshake(&mut io, Handshake::server(&ss).unwrap());
        while matches!(c, Step::Pending) || matches!(s, Step::Pending) {
            io.tick(true, &mut out).unwrap();
            while let Some(done) = out.pop() {
                match done.token() {
                    C_SEND => c = client.on_handshake(&mut io, true, done.result()),
                    C_RECV => c = client.on_handshake(&mut io, false, done.result()),
                    S_SEND => s = server.on_handshake(&mut io, true, done.result()),
                    S_RECV => s = server.on_handshake(&mut io, false, done.result()),
                    other => panic!("unexpected token {other:?}"),
                }
            }
        }
        assert_eq!((c, s), (Step::Done(0), Step::Done(0)));
    }
    Pair { io, client, server }
}

#[cfg(test)]
/// The server's side of a ring receive: plaintext collected piece by piece.
struct Sink {
    got: Vec<u8>,
    closed: bool,
    /// Offer each ring buffer first with a destination that is nearly full.
    tight: bool,
    /// Times the stream kept a whole record with every byte offered taken.
    kept: u32,
}

#[cfg(test)]
impl Sink {
    /// Feed `len` bytes of ring buffer `buf` to the stream, handing the
    /// destination over in pieces.
    fn feed(&mut self, p: &mut Pair, buf: BufId, len: u32) -> Result<(), TlsError> {
        let mut consumed = 0;
        let mut at = if self.tight { DST_PIECE - TIGHT } else { 0 };
        loop {
            let from = RingBytes::new(buf, consumed, len - consumed);
            let dst = RingDst::new(S_DST, at, DST_PIECE - at);
            let step = p.server.on_ring_recv(&mut p.io, from, dst)?;
            let (start, end) = (at as usize, (at + step.produced()) as usize);
            self.got.extend_from_slice(&p.io.buf(S_DST)[start..end]);
            consumed += step.consumed();
            self.closed |= step.closed();
            // The stream ended, or every record there is has been opened.
            if step.closed() || !step.no_room() {
                assert!(
                    step.closed() || consumed == len,
                    "bytes left without a reason"
                );
                return Ok(());
            }
            self.kept += u32::from(consumed == len);
            at = 0;
        }
    }
}

#[cfg(test)]
/// Send `data` from the client; receive it on the server through an armed
/// receive. Returns what arrived, or the error the stream reported.
fn transfer(p: &mut Pair, data: &[u8], close_after: bool) -> Result<Sink, TlsError> {
    transfer_into(p, data, close_after, false)
}

#[cfg(test)]
/// `transfer`, with each ring buffer offered first to a destination that
/// is nearly full when `tight`.
fn transfer_into(
    p: &mut Pair,
    data: &[u8],
    close_after: bool,
    tight: bool,
) -> Result<Sink, TlsError> {
    let mut sink = Sink {
        got: Vec::new(),
        closed: false,
        tight,
        kept: 0,
    };
    // What arrived behind the handshake (session tickets) comes first.
    sink.feed(p, S_DST, 0)?;
    p.io.buf(C_TX)[..data.len()].copy_from_slice(data);
    p.client.send(&mut p.io, C_TX, 0, data.len() as u32, 4096);
    p.io.submit(S_RECV, Op::ArmedRecv { sock: SERVER }).unwrap();
    let mut out = Completions::new(8);
    let mut closing = false;
    while sink.got.len() < data.len() || (close_after && !sink.closed) {
        p.io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            match c.token() {
                C_SEND => match p.client.on_send(&mut p.io, c.result()) {
                    Step::Done(_) if close_after && !closing => {
                        closing = true;
                        p.client.send_alert(&mut p.io, C_TX, 0, 1, 0);
                    }
                    Step::Done(_) | Step::Pending => {}
                    Step::Tls(e) => panic!("send: {e:?}"),
                },
                S_RECV => {
                    if let Some(buf) = c.buf() {
                        let fed = sink.feed(p, buf, c.result().unsigned_abs());
                        p.io.give_back(buf);
                        fed?;
                    } else if c.result() < 0 && c.result() != -105 {
                        return Err(TlsError::BadRecord);
                    }
                    if !c.more() && !sink.closed {
                        p.io.submit(S_RECV, Op::ArmedRecv { sock: SERVER }).unwrap();
                    }
                }
                other => panic!("unexpected token {other:?}"),
            }
        }
    }
    Ok(sink)
}

#[cfg(test)]
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ seed)
        .collect()
}

#[test]
fn tls_records_straddling_ring_buffers_open_to_what_was_sent() {
    // Ring buffers far smaller than a record, about a record, and larger.
    for (ring, len) in [(700, 100_000), (16_384, 150_000), (40_000, 200_000)] {
        let mut p = pair(7, Faults::default(), ring, true);
        let data = pattern(len, 3);
        let sink = transfer(&mut p, &data, false).unwrap();
        assert!(sink.got == data, "ring buffers of {ring}");
    }
}

#[test]
fn a_record_kept_for_want_of_room_is_reported_and_opens_with_no_more_bytes() {
    // Ring buffers far smaller than a record: every record is carried,
    // and is whole exactly when the ring buffer that ends it is taken.
    let mut p = pair(7, Faults::default(), 700, true);
    let data = pattern(100_000, 6);
    // Nothing follows the data, so its last record ends a ring buffer.
    let sink = transfer_into(&mut p, &data, false, true).unwrap();
    assert!(sink.got == data);
    assert!(sink.kept > 0, "no record was kept with all bytes taken");
}

#[test]
fn partial_receives_and_short_sends_change_nothing() {
    for seed in 0..40 {
        let faults = Faults::NONE
            .with_partial_recv(500_000)
            .with_short_send(300_000);
        let mut p = pair(seed, faults, 3000, true);
        let data = pattern(60_000, seed as u8);
        let sink = transfer(&mut p, &data, false).unwrap();
        assert!(sink.got == data, "seed {seed}");
    }
}

#[test]
fn a_close_alert_ends_the_stream_after_all_the_data() {
    let mut p = pair(11, Faults::default(), 5000, true);
    let data = pattern(50_000, 9);
    let sink = transfer(&mut p, &data, true).unwrap();
    assert!(sink.closed && sink.got == data);
}

#[test]
fn a_flipped_bit_is_an_error_and_never_delivered_as_data() {
    let mut failed = 0;
    for seed in 0..20 {
        // Bits flip only once the handshake is over.
        let mut p = pair(seed, Faults::NONE, 4000, true);
        p.io.set_faults(Faults::NONE.with_bit_flip(200_000));
        let data = pattern(80_000, 1);
        match transfer(&mut p, &data, false) {
            Ok(sink) => assert!(sink.got == data, "seed {seed}: corrupt data delivered"),
            Err(_) => failed += 1,
        }
    }
    assert!(failed > 0, "no seed flipped a bit in a record");
}

#[test]
fn a_plain_stream_passes_ring_bytes_through() {
    let mut p = pair(5, Faults::default(), 900, false);
    let data = pattern(30_000, 4);
    let sink = transfer(&mut p, &data, false).unwrap();
    assert!(sink.got == data);
}
