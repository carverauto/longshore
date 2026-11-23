// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the test files in this directory: the simulated world and its helpers.
#![expect(
    dead_code,
    reason = "shared by the test binaries in this directory; each binary uses only part of it"
)]

pub use longshore_common::io::alloc_guard::{sealed, Guard};
pub use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
pub use longshore_common::sim::{Faults, SimIo};
pub use longshore_http_client::client::{self, Client, Done, Error, Handled, Request, Response};
pub use longshore_http_client::server::{self, Server};
pub use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[global_allocator]
static ALLOC: Guard = Guard;

pub const CONNS: u32 = 3;

pub const BUF: u32 = 32 * 1024;

pub const HEAD_MAX: u32 = 2048;

pub const MS: u64 = 1_000_000;

pub const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 80)), 80);

pub const HEARTBEAT: u64 = 1 << 50;

// Buffers: client heads 0..3, server (head, body) pairs 3..9, then user buffers.
pub const TX: BufId = BufId(9);

pub const RX: BufId = BufId(10);

// Two more sockets and one more op for a hand-driven peer.
pub const LIMITS: Limits = Limits {
    sockets_max: 1 + CONNS * 2 + 2,
    buffers: 11,
    buffer_size: BUF,
    ops_max: CONNS * 2 + CONNS + 1 + 1 + 1,
    ..Limits::BASE
};

pub struct World {
    pub io: SimIo,
    pub client: Client,
    pub server: Server,
    pub out: Completions,
    pub done: Vec<Done>,
}

pub fn client_cfg() -> client::Config {
    client::Config {
        connections_max: CONNS,
        first_sock: 1,
        first_buf: 0,
        head_max: HEAD_MAX,
        buffer_size: BUF,
        max_age_ns: 10_000 * MS,
        max_requests: 1000,
        request_timeout_ns: 2_000 * MS,
        zero_copy_min: 4096,
        token_base: 1 << 40,
        tls_batch: 256 * 1024,
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    }
}

pub fn world_with(seed: u64, faults: Faults, cfg: client::Config) -> World {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: CONNS,
        head_max: HEAD_MAX,
        buffer_size: BUF,
        objects_max: 4,
        token_base: 2 << 40,
        checksum: None,
    });
    server.start(&mut io, ADDR).unwrap();
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    World {
        io,
        client: Client::new(cfg),
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(64),
    }
}

pub fn world(seed: u64, faults: Faults) -> World {
    world_with(seed, faults, client_cfg())
}

impl World {
    /// Run until `n` requests have finished in total. The owner's heartbeat
    /// drives client timeouts, as a real event loop would.
    pub fn run_until_done(&mut self, n: usize) {
        let World {
            io,
            client,
            server,
            out,
            done,
        } = self;
        sealed(|| {
            while done.len() < n {
                io.tick(true, out).expect("simulation deadlock");
                while let Some(c) = out.pop() {
                    if c.token.0 == HEARTBEAT {
                        client.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                        continue;
                    }
                    match client.on_completion(io, c) {
                        Handled::Done(d) => done.push(d),
                        Handled::Pending => {}
                        Handled::NotMine => {
                            assert!(server.on_completion(io, c), "unrouted completion {c:?}")
                        }
                    }
                }
            }
        })
    }

    /// Let both sides settle (closes, keep-alive bookkeeping) for `ms` of virtual time.
    pub fn settle(&mut self, ms: u64) {
        let until = self.io.now_ns() + ms * MS;
        let World {
            io,
            client,
            server,
            out,
            done,
        } = self;
        sealed(|| {
            while io.now_ns() < until {
                io.tick(true, out).expect("simulation deadlock");
                while let Some(c) = out.pop() {
                    if c.token.0 == HEARTBEAT {
                        client.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                    } else if let Handled::Done(d) = match client.on_completion(io, c) {
                        Handled::NotMine => {
                            server.on_completion(io, c);
                            Handled::Pending
                        }
                        h => h,
                    } {
                        done.push(d);
                    }
                }
            }
        })
    }

    pub fn start(
        &mut self,
        token: u64,
        method: &str,
        target: &str,
        body: Option<u32>,
    ) -> Result<(), Error> {
        let req = Request {
            method,
            target,
            host: "objects.test",
            host_id: 0,
            addr: ADDR,
            headers: &[("x-test", b"1")],
            body: body.map(|n| (TX, n)),
            response_buf: Some(RX),
        };
        self.client.start(&mut self.io, token, &req)
    }

    /// One request, start to finish.
    pub fn request(
        &mut self,
        method: &str,
        target: &str,
        body: Option<u32>,
    ) -> Result<Response, Error> {
        let token = self.done.len() as u64 + 100;
        self.start(token, method, target, body)
            .expect("a connection should be free");
        self.run_until_done(self.done.len() + 1);
        let d = *self.done.last().unwrap();
        assert_eq!(d.token, token);
        d.result
    }

    pub fn fill_tx(&mut self, len: u32, pattern: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ pattern;
        }
    }
}

pub fn ok(status: u16, body_len: u32) -> impl Fn(Result<Response, Error>) {
    move |r| match r {
        Ok(resp) => assert_eq!((resp.status, resp.body_len), (status, body_len)),
        Err(e) => panic!("expected {status}, got {e:?}"),
    }
}

/// What a hand-driven peer saw and what the client made of its answer.
pub struct RawExchange {
    pub result: Result<Response, Error>,
    /// The request exactly as the client put it on the wire.
    pub request: Vec<u8>,
}

/// The address of the hand-driven peer.
pub const RAW_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 82)), 80);

/// One request to a hand-driven peer that reads the request, answers with
/// exactly `reply`, and then neither sends more nor closes. Unlike the
/// in-tree server it shares no parsing code with the client, so what it
/// records is an independent view of the client's bytes.
pub fn raw_exchange(
    cfg: client::Config,
    method: &str,
    headers: &[(&str, &[u8])],
    response_buf: Option<BufId>,
    reply: &[u8],
) -> RawExchange {
    const LISTEN: Sock = Sock(1 + CONNS * 2);
    const PEER_SOCK: Sock = Sock(2 + CONNS * 2);
    const ACCEPT: u64 = 3 << 40;
    const RECV: u64 = ACCEPT + 1;
    const SEND: u64 = ACCEPT + 2;
    const IN_MAX: u32 = 4096;
    let mut w = world_with(12, Faults::default(), cfg);
    w.io.listen(LISTEN, RAW_PEER).unwrap();
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
    let (mut sent, mut got, mut result) = (false, 0u32, None);
    let World {
        io,
        client,
        server,
        out,
        ..
    } = &mut w;
    let recv = |io: &mut SimIo, got: u32| {
        io.submit(
            Token(RECV),
            Op::Recv {
                sock: PEER_SOCK,
                buf: TX,
                off: got,
                len: IN_MAX - got,
            },
        )
        .unwrap()
    };
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
                    ACCEPT => recv(io, got),
                    RECV if c.result > 0 => {
                        got += c.result as u32;
                        if !sent {
                            io.submit(
                                Token(SEND),
                                Op::Send {
                                    sock: PEER_SOCK,
                                    buf: TX,
                                    off: IN_MAX,
                                    len: reply.len() as u32,
                                    zero_copy: false,
                                },
                            )
                            .unwrap();
                            sent = true;
                        }
                        recv(io, got);
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
    RawExchange {
        result: result.unwrap(),
        request: w.io.buf(TX)[..got as usize].to_vec(),
    }
}
