// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the test files in this directory: the simulated world and its helpers.
#![expect(
    dead_code,
    unused_imports,
    reason = "a prelude and helpers shared by the store and client test binaries; each binary uses only part of it"
)]

pub use longshore_common::io::alloc_guard::{sealed, Guard};
pub use longshore_common::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
pub use longshore_common::sim::{Faults, SimIo};
pub use longshore_http_client::client;
pub use longshore_http_client::server::{self, Behavior, Server};
pub use longshore_s3::creds::Credentials;
pub use longshore_s3::list::{unescape, Listing};
pub use longshore_s3::s3::{Checksum, Config, Done, Endpoint, Fatal, Outcome, SubmitError, S3};
pub use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub mod names;
pub mod parts;
pub mod signing;

#[global_allocator]
static ALLOC: Guard = Guard;

pub const CONNS: u32 = 3;

pub const BUF: u32 = 16 * 1024;

pub const MS: u64 = 1_000_000;

pub const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 80)), 80);

pub const HEARTBEAT: u64 = 1 << 50;

// Large buffers: server pairs 0..6, user tx 6, rx 7. Small buffers: client heads 8..11.
pub const TX: BufId = BufId::new(6);

pub const RX: BufId = BufId::new(7);

pub const HEAD_MAX: u32 = 8192;

pub const LIMITS: Limits = Limits::BASE
    .with_sockets_max(1 + CONNS * 2)
    .with_buffers(8)
    .with_buffer_size(BUF)
    .with_small_buffers(CONNS)
    .with_small_buffer_size(HEAD_MAX)
    .with_ops_max(CONNS * 3 + 2);

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
    let cfg = server::ServerConfig::builder(Sock::new(0), CONNS, 1 + CONNS, 0, BUF, 2 << 40)
        .head_max(HEAD_MAX)
        .objects_max(8);
    let cfg = if enforce {
        cfg.checksum(|body: &[u8]| {
            longshore_crc::crc64_header_value(longshore_crc::crc64_nvme(body))
        })
    } else {
        cfg
    };
    let mut server = Server::new(cfg.build().unwrap());
    // Every request must carry a signature that matches the bytes it
    // arrived as; see `signing`.
    server.verify_requests(signing::verify_sigv4);
    let _: SocketAddr = server.start(&mut io, ADDR).unwrap();
    finish_world(io, server, s3_config(attempts_max, enforce))
}

/// Start a store with `cfg` beside a server that is already listening.
pub fn finish_world(mut io: SimIo, server: Server, cfg: Config) -> World {
    let mut s3 = S3::new(cfg);
    s3.start(&mut io);
    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
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
                    if c.token().get() == HEARTBEAT {
                        s3.tick(io);
                        io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
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

    pub fn fill_tx(&mut self, len: u32, pattern: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ pattern;
        }
    }
}

/// The HTTP client configuration [`s3_config`] uses, with its sockets,
/// head buffers and tokens starting where the caller says.
pub fn http_config(first_sock: u32, first_buf: u32, token_base: u64) -> client::ClientConfig {
    client::ClientConfig::builder(CONNS, first_sock, first_buf, BUF, token_base)
        .head_max(HEAD_MAX)
        .max_age_ns(60_000 * MS)
        .max_requests(1000)
        .request_timeout_ns(1_000 * MS)
        .zero_copy_min(4096)
        .tls_batch(256 * 1024)
        .build()
        .unwrap()
}

/// The store configuration [`world_with`] uses.
pub fn s3_config(attempts_max: u32, enforce: bool) -> Config {
    let http = http_config(1, LIMITS.buffers(), 1 << 40);
    Config::new(
        longshore_s3::s3::Vendor::S3,
        "bucket.objects.test".into(),
        "us-test-1".into(),
        Credentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY").unwrap(),
        http,
        Endpoint::Fixed(ADDR),
    )
    .with_bucket_in_path(None)
    .with_port(80)
    .with_credentials_file(None)
    .with_requests_max(4)
    .with_attempts_max(attempts_max)
    .with_backoff_ns(100 * MS)
    .with_backoff_max_ns(800 * MS)
    .with_require_checksums(enforce)
}
