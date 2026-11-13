// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the test files in this directory: the simulated world and its helpers.
#![allow(dead_code, unused_imports)]

pub use longshore::http::client;
pub use longshore::http::server::{self, Behavior, Server};
pub use longshore::io::alloc_guard::{sealed, Guard};
pub use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
pub use longshore::sim::{Faults, SimIo};
pub use longshore::store::creds::Credentials;
pub use longshore::store::list::{unescape, Listing};
pub use longshore::store::s3::{Checksum, Config, Done, Endpoint, Fatal, Outcome, SubmitError, S3};
pub use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

pub const CONNS: u32 = 3;

pub const BUF: u32 = 16 * 1024;

pub const MS: u64 = 1_000_000;

pub const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 80), 80);

pub const HEARTBEAT: u64 = 1 << 50;

// Large buffers: server pairs 0..6, user tx 6, rx 7. Small buffers: client heads 8..11.
pub const TX: BufId = BufId(6);

pub const RX: BufId = BufId(7);

pub const HEAD_MAX: u32 = 2048;

pub const LIMITS: Limits = Limits {
    sockets_max: 1 + CONNS * 2,
    buffers: 8,
    buffer_size: BUF,
    small_buffers: CONNS,
    small_buffer_size: HEAD_MAX,
    ops_max: CONNS * 3 + 2,
};

pub struct World {
    pub io: SimIo,
    pub s3: S3,
    pub server: Server,
    pub out: Completions,
    pub done: Vec<Done>,
}

pub fn world(seed: u64, faults: Faults, attempts_max: u32) -> World {
    world_with(seed, faults, attempts_max, false)
}

/// `enforce`: the server checks and returns CRC-64/NVME checksums, as AWS does.
pub fn world_with(seed: u64, faults: Faults, attempts_max: u32, enforce: bool) -> World {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 0,
        head_max: 2048,
        buffer_size: BUF,
        objects_max: 8,
        token_base: 2 << 40,
        checksum: enforce.then_some(|body: &[u8]| {
            longshore::checksum::crc64_header_value(longshore::checksum::crc64_nvme(body))
        }),
    });
    server.start(&mut io, ADDR).unwrap();
    let http = client::Config {
        connections_max: CONNS,
        first_sock: 1,
        first_buf: LIMITS.buffers,
        head_max: HEAD_MAX,
        buffer_size: BUF,
        max_age_ns: 60_000 * MS,
        max_requests: 1000,
        request_timeout_ns: 1_000 * MS,
        zero_copy_min: 4096,
        token_base: 1 << 40,
        tls_batch: 256 * 1024,
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    };
    let mut s3 = S3::new(Config {
        vendor: longshore::store::s3::Vendor::S3,
        host: "bucket.objects.test".into(),
        port: 80,
        region: "us-test-1".into(),
        credentials: Credentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY")
            .unwrap(),
        credentials_file: None,
        requests_max: 4,
        attempts_max,
        backoff_ns: 100 * MS,
        backoff_max_ns: 800 * MS,
        require_checksums: enforce,
        http,
        tls: None,
        endpoint: Endpoint::Fixed(ADDR),
    });
    s3.start(&mut io);
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    World {
        io,
        s3,
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(64),
    }
}

impl World {
    /// Run until `n` operations have finished in total, with allocation sealed.
    pub fn run_until_done(&mut self, n: usize) {
        let World {
            io,
            s3,
            server,
            out,
            done,
        } = self;
        sealed(|| {
            while done.len() < n {
                io.tick(true, out).expect("simulation deadlock");
                while let Some(c) = out.pop() {
                    if c.token.0 == HEARTBEAT {
                        s3.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                    } else if !s3.on_completion(io, c) {
                        assert!(server.on_completion(io, c), "unrouted completion {c:?}");
                    }
                }
                while let Some(d) = s3.poll() {
                    done.push(d);
                }
            }
        })
    }

    pub fn last(&mut self) -> Done {
        self.run_until_done(self.done.len() + 1);
        *self.done.last().unwrap()
    }

    pub fn fill_tx(&mut self, len: u32, salt: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ salt;
        }
    }
}
