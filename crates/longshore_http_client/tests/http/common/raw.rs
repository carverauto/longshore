// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A hand-driven peer: reads one request and answers with exact bytes.

use super::{
    client, sealed, world_with, BufId, Error, Faults, Handled, Io, Op, Request, Response, SimIo,
    Sock, Token, World, BUF, CONNS, HEARTBEAT, MS, RX, TX,
};
use longshore_common::io::Completion;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// What a hand-driven peer saw and what the client made of its answer.
pub struct RawExchange {
    pub result: Result<Response, Error>,
    /// The request exactly as the client put it on the wire.
    pub request: Vec<u8>,
}

/// The address of the hand-driven peer.
pub const RAW_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 82)), 80);

const LISTEN: Sock = Sock(1 + CONNS * 2);
const PEER_SOCK: Sock = Sock(2 + CONNS * 2);
const ACCEPT: u64 = 3 << 40;
const RECV: u64 = ACCEPT + 1;
const SEND: u64 = ACCEPT + 2;
/// The request lands in `TX` before this offset; the reply waits after it.
const IN_MAX: u32 = 4096;

/// One request to a hand-driven peer that reads the request, answers with
/// exactly `reply`, and then neither sends more nor closes. Unlike the
/// in-tree server it shares no parsing code with the client, so what it
/// records is an independent view of the client's bytes.
pub fn raw_exchange(
    cfg: client::ClientConfig,
    method: &str,
    headers: &[(&str, &[u8])],
    response_buf: Option<BufId>,
    reply: &[u8],
) -> RawExchange {
    let mut w = world_with(12, Faults::default(), cfg);
    let _: SocketAddr = w.io.listen(LISTEN, RAW_PEER).unwrap();
    w.io.submit(
        Token(ACCEPT),
        Op::Accept {
            listener: LISTEN,
            sock: PEER_SOCK,
        },
    )
    .unwrap();
    assert!(reply.len() <= (BUF - IN_MAX) as usize);
    w.io.buf(TX)[IN_MAX as usize..IN_MAX as usize + reply.len()].copy_from_slice(reply);
    let req = Request {
        method,
        target: "/raw",
        host: "raw.test",
        host_id: 2,
        addr: RAW_PEER,
        headers,
        body: None,
        response_buf,
    };
    w.client.start(&mut w.io, 9, &req).unwrap();
    let mut peer = RawPeer {
        sent: false,
        got: 0,
        reply_len: reply.len() as u32,
    };
    let result = run_until_answered(&mut w, &mut peer);
    RawExchange {
        result,
        request: w.io.buf(TX)[..peer.got as usize].to_vec(),
    }
}

/// Drives the world until the client finishes its one request; the raw
/// peer's completions go to `peer`.
fn run_until_answered(w: &mut World, peer: &mut RawPeer) -> Result<Response, Error> {
    let mut result = None;
    let World {
        io,
        client,
        server,
        out,
        ..
    } = w;
    sealed(|| {
        while result.is_none() {
            io.tick(true, out).expect("simulation deadlock");
            while let Some(c) = out.pop() {
                match c.token.0 {
                    HEARTBEAT => {
                        client.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                    }
                    ACCEPT | RECV | SEND => peer.on(io, c),
                    _ => match client.on_completion(io, c) {
                        Handled::Done(d) => result = Some(d.result),
                        Handled::Pending => {}
                        Handled::NotMine => {
                            assert!(server.on_completion(io, c), "unrouted completion {c:?}")
                        }
                    },
                }
            }
        }
    });
    result.unwrap()
}

/// The peer's side: what it has read, and whether it has answered.
struct RawPeer {
    sent: bool,
    got: u32,
    reply_len: u32,
}

impl RawPeer {
    fn on(&mut self, io: &mut SimIo, c: Completion) {
        match c.token.0 {
            ACCEPT => self.recv(io),
            RECV if c.result > 0 => {
                self.got += u32::try_from(c.result).unwrap();
                if !self.sent {
                    let send = Op::Send {
                        sock: PEER_SOCK,
                        buf: TX,
                        off: IN_MAX,
                        len: self.reply_len,
                        zero_copy: false,
                    };
                    io.submit(Token(SEND), send).unwrap();
                    self.sent = true;
                }
                self.recv(io);
            }
            _ => {}
        }
    }

    fn recv(&self, io: &mut SimIo) {
        let at = self.got;
        let recv = Op::Recv {
            sock: PEER_SOCK,
            buf: TX,
            off: at,
            len: IN_MAX - at,
        };
        io.submit(Token(RECV), recv).unwrap();
    }
}

/// One GET to a hand-driven peer that answers the request with `reply`
/// and then waits for the client to close. The peer reads into the same
/// 1 KiB at the start of `TX` each time.
pub fn raw_reply_with(cfg: client::ClientConfig, reply: &'static [u8]) -> Result<Response, Error> {
    let mut w = world_with(12, Faults::default(), cfg);
    let _: SocketAddr = w.io.listen(LISTEN, RAW_PEER).unwrap();
    w.io.submit(
        Token(ACCEPT),
        Op::Accept {
            listener: LISTEN,
            sock: PEER_SOCK,
        },
    )
    .unwrap();
    w.io.buf(TX)[IN_MAX as usize..IN_MAX as usize + reply.len()].copy_from_slice(reply);
    let req = Request {
        method: "GET",
        target: "/raw",
        host: "raw.test",
        host_id: 2,
        addr: RAW_PEER,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    w.client.start(&mut w.io, 9, &req).unwrap();
    run_with_raw_peer(&mut w, reply.len() as u32)
}

/// Drives the world until the client's request ends. The peer reads, and
/// on its first data answers with the `reply_len` bytes at `IN_MAX`.
fn run_with_raw_peer(w: &mut World, reply_len: u32) -> Result<Response, Error> {
    let (mut sent, mut result) = (false, None);
    let World {
        io,
        client,
        server,
        out,
        ..
    } = w;
    sealed(|| {
        while result.is_none() {
            io.tick(true, out).expect("simulation deadlock");
            while let Some(c) = out.pop() {
                match c.token.0 {
                    HEARTBEAT => {
                        client.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                    }
                    ACCEPT => peer_recv(io),
                    RECV if c.result > 0 => {
                        if !sent {
                            let send = Op::Send {
                                sock: PEER_SOCK,
                                buf: TX,
                                off: IN_MAX,
                                len: reply_len,
                                zero_copy: false,
                            };
                            io.submit(Token(SEND), send).unwrap();
                            sent = true;
                        }
                        peer_recv(io);
                    }
                    RECV | SEND => {}
                    _ => match client.on_completion(io, c) {
                        Handled::Done(d) => result = Some(d.result),
                        Handled::Pending => {}
                        Handled::NotMine => {
                            assert!(server.on_completion(io, c), "unrouted completion {c:?}")
                        }
                    },
                }
            }
        }
    });
    result.unwrap()
}

/// The peer reads up to 1 KiB more into the start of `TX`.
fn peer_recv(io: &mut SimIo) {
    let recv = Op::Recv {
        sock: PEER_SOCK,
        buf: TX,
        off: 0,
        len: 1024,
    };
    io.submit(Token(RECV), recv).unwrap();
}
