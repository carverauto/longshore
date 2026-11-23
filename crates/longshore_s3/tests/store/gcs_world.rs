// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared by the Google Cloud Storage test files: the simulated world, with
//! a server that speaks that dialect and a token file to rotate.

pub use longshore_common::io::alloc_guard::{sealed, Guard};
pub use longshore_common::io::{BufId, Completions, FileId, Io, Limits, Op, Sock, Token};
pub use longshore_common::sim::{Faults, SimIo};
pub use longshore_http_client::client;
pub use longshore_http_client::server::{self, Behavior, Server};
pub use longshore_s3::creds::Credentials;
pub use longshore_s3::s3::{
    Checksum, Config, CredentialsFile, Done, Endpoint, Fatal, Outcome, Vendor, S3,
};
pub use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const CONNS: u32 = 3;
pub const BUF: u32 = 16 * 1024;
pub const HEAD_MAX: u32 = 4096;
pub const MS: u64 = 1_000_000;
pub const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 80)), 80);
pub const HEARTBEAT: u64 = 1 << 50;
pub const REREAD_MS: u64 = 200;
// Large buffers: server pairs 0..6, user tx 6, rx 7, token file 8. Small buffers: client heads.
pub const TX: BufId = BufId::new(6);
pub const RX: BufId = BufId::new(7);
pub const TOKEN_BUF: BufId = BufId::new(8);
pub const LIMITS: Limits = Limits::BASE
    .with_sockets_max(1 + CONNS * 2)
    .with_buffers(9)
    .with_buffer_size(BUF)
    .with_small_buffers(CONNS)
    .with_small_buffer_size(HEAD_MAX)
    .with_ops_max(CONNS * 3 + CONNS + 1 + 2);

pub const TOKEN_A: &str = "ya29.first-token_0123456789abcdefghijklmnopqrstuvwxyz";
pub const TOKEN_B: &str = "ya29.second-token_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210";

/// CRC-32C as the server's checksum hook carries it: eight base64 characters, then zeros.
pub fn crc32c_form(body: &[u8]) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[..8].copy_from_slice(&longshore_crc::crc32c_header_value(longshore_crc::crc32c(
        body,
    )));
    out
}

pub struct World {
    pub io: SimIo,
    pub s3: S3,
    pub server: Server,
    pub out: Completions,
    pub done: Vec<Done>,
    pub token_file: FileId,
}

pub fn world(seed: u64, faults: Faults, attempts_max: u32) -> World {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut server = Server::new(
        server::ServerConfig::builder(Sock::new(0), CONNS, 1 + CONNS, 0, BUF, 2 << 40)
            .head_max(HEAD_MAX)
            .objects_max(8)
            .checksum(crc32c_form)
            .build()
            .unwrap(),
    );
    let _: SocketAddr = server.start(&mut io, ADDR).unwrap();
    server.allow_access_keys(&[TOKEN_A]);
    let token_file = io.register_file("/var/run/token").unwrap();
    let http = client::ClientConfig::builder(CONNS, 1, LIMITS.buffers(), BUF, 1 << 40)
        .head_max(HEAD_MAX)
        .max_age_ns(60_000 * MS)
        .max_requests(1000)
        .request_timeout_ns(1_000 * MS)
        .zero_copy_min(4096)
        .tls_batch(256 * 1024)
        .build()
        .unwrap();
    let mut s3 = S3::new(
        Config::new(
            Vendor::Gcs,
            "bucket.storage.test".into(),
            "auto".into(),
            Credentials::bearer(TOKEN_A).unwrap(),
            http,
            Endpoint::Fixed(ADDR),
        )
        .with_bucket_in_path(None)
        .with_port(80)
        .with_credentials_file(Some(CredentialsFile::new(
            token_file,
            TOKEN_BUF,
            REREAD_MS * MS,
            1 << 51,
            longshore_s3::s3::Validate::List(longshore_s3::s3::KeyPrefix::EMPTY),
        )))
        .with_requests_max(4)
        .with_attempts_max(attempts_max)
        .with_backoff_ns(50 * MS)
        .with_backoff_max_ns(400 * MS)
        .with_require_checksums(true),
    );
    s3.start(&mut io);
    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    World {
        io,
        s3,
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(256),
        token_file,
    }
}

impl World {
    pub fn run(&mut self, n: usize, until: u64) {
        let World {
            io,
            s3,
            server,
            out,
            done,
            ..
        } = self;
        sealed(|| {
            while done.len() < n || io.now_ns() < until {
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
        self.run(self.done.len() + 1, 0);
        *self.done.last().unwrap()
    }

    pub fn settle(&mut self, ms: u64) {
        let until = self.io.now_ns() + ms * MS;
        self.run(self.done.len(), until);
    }

    pub fn fill_tx(&mut self, len: u32, pattern: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ pattern;
        }
    }
}
