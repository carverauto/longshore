// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the test files in this directory: the simulated world and its helpers.
#![allow(dead_code, unused_imports)]

pub use osc_http::client::{self, Client, Done, Error, Handled, Request, Response};
pub use osc_http::head::HttpError;
pub use osc_http::server::{self, Behavior, Server};
pub use osc_io::alloc_guard::{sealed, Guard};
pub use osc_io::{BufId, Completions, Io, Limits, Op, Sock, Token};
pub use osc_sim::{Faults, SimIo};
pub use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

pub const CONNS: u32 = 3;

pub const BUF: u32 = 32 * 1024;

pub const HEAD_MAX: u32 = 2048;

pub const MS: u64 = 1_000_000;

pub const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 80), 80);

pub const HEARTBEAT: u64 = 1 << 50;

// Buffers: client heads 0..3, server (head, body) pairs 3..9, then user buffers.
pub const TX: BufId = BufId(9);

pub const RX: BufId = BufId(10);

// Two more sockets and one more op for a hand-driven peer.
pub const LIMITS: Limits = Limits {
    sockets_max: 1 + CONNS * 2 + 2,
    buffers: 11,
    buffer_size: BUF,
    small_buffers: 0,
    small_buffer_size: 0,
    ops_max: CONNS * 2 + CONNS + 1 + 1 + 1,
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

    pub fn fill_tx(&mut self, len: u32, salt: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ salt;
        }
    }
}

pub fn ok(status: u16, body_len: u32) -> impl Fn(Result<Response, Error>) {
    move |r| match r {
        Ok(resp) => assert_eq!((resp.status, resp.body_len), (status, body_len)),
        Err(e) => panic!("expected {status}, got {e:?}"),
    }
}
