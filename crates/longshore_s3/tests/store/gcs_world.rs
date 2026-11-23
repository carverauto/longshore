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
pub const TX: BufId = BufId(6);
pub const RX: BufId = BufId(7);
pub const TOKEN_BUF: BufId = BufId(8);
pub const LIMITS: Limits = Limits {
    sockets_max: 1 + CONNS * 2,
    buffers: 9,
    buffer_size: BUF,
    small_buffers: CONNS,
    small_buffer_size: HEAD_MAX,
    ops_max: CONNS * 3 + CONNS + 1 + 2,
    ..Limits::BASE
};

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
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 0,
        head_max: HEAD_MAX,
        buffer_size: BUF,
        objects_max: 8,
        token_base: 2 << 40,
        checksum: Some(crc32c_form),
    });
    server.start(&mut io, ADDR).unwrap();
    server.allow_access_keys(&[TOKEN_A]);
    let token_file = io.register_file("/var/run/token").unwrap();
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
        vendor: Vendor::Gcs,
        host: "bucket.storage.test".into(),
        bucket_in_path: None,
        port: 80,
        region: "auto".into(),
        credentials: Credentials::bearer(TOKEN_A).unwrap(),
        credentials_file: Some(CredentialsFile {
            file: token_file,
            buf: TOKEN_BUF,
            reread_ns: REREAD_MS * MS,
            token: 1 << 51,
            validate: longshore_s3::s3::Validate::List(longshore_s3::s3::KeyPrefix::EMPTY),
        }),
        requests_max: 4,
        attempts_max,
        backoff_ns: 50 * MS,
        backoff_max_ns: 400 * MS,
        require_checksums: true,
        http,
        #[cfg(feature = "tls")]
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
