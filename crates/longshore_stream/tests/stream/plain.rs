// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulator tests for plain `Stream` (runs with and without the `tls` feature).

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_stream::{IdleCheck, Step, Stream, TlsError, KEY_RECORDS_RETIRE};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 8080);
const LISTENER: Sock = Sock::new(0);
const CLIENT: Sock = Sock::new(1);
const SERVER: Sock = Sock::new(2);

const C_CONN: Token = Token::new(1);
const C_SEND: Token = Token::new(2);
const C_RECV: Token = Token::new(3);
const S_ACCEPT: Token = Token::new(4);
const S_SEND: Token = Token::new(5);
const S_RECV: Token = Token::new(6);
const C_CLOSE: Token = Token::new(7);
const S_CLOSE: Token = Token::new(8);

const C_TX: BufId = BufId::new(0);
const C_RX: BufId = BufId::new(1);
const S_BUF: BufId = BufId::new(2);
const SCRATCH: BufId = BufId::new(3);

const LIMITS: Limits = Limits::BASE
    .with_sockets_max(3)
    .with_buffers(4)
    .with_buffer_size(8192)
    .with_ops_max(8);

#[cfg(test)]
fn connect_pair(seed: u64, faults: Faults) -> (SimIo, Stream, Stream) {
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
                if c.token() == C_CONN {
                    assert!(c.result() >= 0, "connect failed: {}", c.result());
                    c_up = true;
                } else if c.token() == S_ACCEPT {
                    assert!(c.result() >= 0, "accept failed: {}", c.result());
                    s_up = true;
                }
            }
        }
    });
    (
        io,
        Stream::new(CLIENT, C_SEND, C_RECV),
        Stream::new(SERVER, S_SEND, S_RECV),
    )
}

/// Progress of an echo: the client sends `total` bytes, the server reads
/// them all and sends them back.
#[cfg(test)]
struct Echo {
    total: u32,
    c_sent: u32,
    c_recvd: u32,
    s_recvd: u32,
    s_sent: u32,
}

#[cfg(test)]
impl Echo {
    fn new(total: u32) -> Self {
        Echo {
            total,
            c_sent: 0,
            c_recvd: 0,
            s_recvd: 0,
            s_sent: 0,
        }
    }

    fn on(&mut self, io: &mut SimIo, client: &mut Stream, server: &mut Stream, c: Completion) {
        let total = self.total;
        match c.token() {
            C_SEND => {
                self.c_sent += sent(client.on_send(io, c.result()));
                if self.c_sent < total {
                    client.send(io, C_TX, self.c_sent, total - self.c_sent, u32::MAX);
                }
            }
            S_RECV => {
                self.s_recvd += received(server.on_recv(io, c.result()));
                if self.s_recvd < total {
                    assert_eq!(
                        server.recv(io, S_BUF, self.s_recvd, total - self.s_recvd),
                        Step::Pending
                    );
                } else {
                    server.send(io, S_BUF, 0, total, 1024);
                    assert_eq!(client.recv(io, C_RX, 0, total), Step::Pending);
                }
            }
            S_SEND => {
                self.s_sent += sent(server.on_send(io, c.result()));
                if self.s_sent < total {
                    server.send(io, S_BUF, self.s_sent, total - self.s_sent, 1024);
                }
            }
            C_RECV => {
                self.c_recvd += received(client.on_recv(io, c.result()));
                if self.c_recvd < total {
                    assert_eq!(
                        client.recv(io, C_RX, self.c_recvd, total - self.c_recvd),
                        Step::Pending
                    );
                }
            }
            other => panic!("unexpected token {other:?}"),
        }
    }
}

/// The bytes a plain send moved; a plain send completes at once.
#[cfg(test)]
fn sent(step: Step) -> u32 {
    let Step::Done(n) = step else {
        panic!("plain send must complete immediately");
    };
    assert!(n > 0);
    u32::try_from(n).unwrap()
}

/// The bytes a plain recv moved; a plain recv completes at once.
#[cfg(test)]
fn received(step: Step) -> u32 {
    let Step::Done(n) = step else {
        panic!("plain recv must complete immediately");
    };
    assert!(n > 0);
    u32::try_from(n).unwrap()
}

#[test]
fn plain_stream_echo_survives_short_writes_and_partial_reads() {
    let faults = Faults::NONE
        .with_short_send(300_000)
        .with_partial_recv(300_000)
        .with_delay(200_000);
    let (mut io, mut client, mut server) = connect_pair(7, faults);
    assert!(!client.is_tls());
    assert!(!client.has_unread_plaintext());
    assert_eq!(client.records(), 0);

    let payload: Vec<u8> = (0..5000u32).map(|i| (i * 31) as u8).collect();
    io.buf(C_TX)[..payload.len()].copy_from_slice(&payload);

    let mut out = Completions::new(8);
    sealed(|| {
        let mut echo = Echo::new(payload.len() as u32);
        client.send(&mut io, C_TX, 0, echo.total, u32::MAX);
        assert_eq!(server.recv(&mut io, S_BUF, 0, echo.total), Step::Pending);

        while echo.c_recvd < echo.total {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                echo.on(&mut io, &mut client, &mut server, c);
            }
        }
    });

    assert_eq!(&io.buf(C_RX)[..payload.len()], &payload[..]);
}

#[test]
fn plain_stream_idle_check_eof_and_reset() {
    let (mut io, mut client, mut server) = connect_pair(19, Faults::default());
    let mut out = Completions::new(8);

    sealed(|| {
        // 1. Silent connection -> Clean.
        assert_eq!(client.check_idle(&mut io, SCRATCH, 0), IdleCheck::Pending);
        io.tick(true, &mut out).unwrap();
        let c = out.pop().unwrap();
        assert_eq!(c.token(), C_RECV);
        assert_eq!(client.on_idle_check(&mut io, c.result()), IdleCheck::Clean);

        // 2. Unsolicited byte -> Dirty.
        io.buf(S_BUF)[0] = b'!';
        server.send(&mut io, S_BUF, 0, 1, u32::MAX);
        io.tick(true, &mut out).unwrap();
        let sent = out.pop().unwrap();
        assert_eq!(sent.token(), S_SEND);
        assert_eq!(server.on_send(&mut io, sent.result()), Step::Done(1));

        assert_eq!(client.check_idle(&mut io, SCRATCH, 0), IdleCheck::Pending);
        io.tick(true, &mut out).unwrap();
        let dirty = out.pop().unwrap();
        assert_eq!(dirty.token(), C_RECV);
        assert_eq!(
            client.on_idle_check(&mut io, dirty.result()),
            IdleCheck::Dirty
        );

        // 3. Peer closes -> check_idle sees Closed, recv sees Done(0).
        io.submit(S_CLOSE, Op::Close { sock: SERVER }).unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().token(), S_CLOSE);

        assert_eq!(client.check_idle(&mut io, SCRATCH, 0), IdleCheck::Pending);
        io.tick(true, &mut out).unwrap();
        let closed = out.pop().unwrap();
        assert_eq!(
            client.on_idle_check(&mut io, closed.result()),
            IdleCheck::Closed
        );

        assert_eq!(client.recv(&mut io, C_RX, 0, 16), Step::Pending);
        io.tick(true, &mut out).unwrap();
        let eof = out.pop().unwrap();
        assert_eq!(client.on_recv(&mut io, eof.result()), Step::Done(0));

        client.reset();
        assert!(!client.is_tls());
        assert_eq!(client.records(), 0);
        io.submit(C_CLOSE, Op::Close { sock: CLIENT }).unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().token(), C_CLOSE);
    });

    // TlsError and KEY_RECORDS_RETIRE are available even when `tls` is disabled.
    assert_eq!(KEY_RECORDS_RETIRE, 1 << 23);
    let _: TlsError = TlsError::BadRecord;
    let _: TlsError = TlsError::Record;
    let _: TlsError = TlsError::Alert(116);
}

#[test]
fn plain_stream_send_alert_submits_zero_len_and_error_passes_through() {
    let (mut io, mut client, mut server) = connect_pair(88, Faults::default());
    let mut out = Completions::new(8);
    sealed(|| {
        client.send_alert(&mut io, C_TX, 0, 1, 0);
        io.tick(true, &mut out).unwrap();
        let c = out.pop().unwrap();
        assert_eq!(c.token(), C_SEND);
        assert_eq!(client.on_send(&mut io, c.result()), Step::Done(0));

        assert_eq!(
            client.on_send(&mut io, -longshore_common::io::EPIPE),
            Step::Done(-longshore_common::io::EPIPE)
        );
        assert_eq!(
            server.on_recv(&mut io, -longshore_common::io::ECONNRESET),
            Step::Done(-longshore_common::io::ECONNRESET)
        );
    });
}
