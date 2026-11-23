// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A store that rereads a credentials file, beside the in-tree server on
//! one simulated `Io`, shared by the credential tests.

use longshore_common::io::alloc_guard::sealed;
use longshore_common::io::{BufId, Completions, FileId, Io, Limits, Op, Sock, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_http_client::client;
use longshore_http_client::server::{self, Server};
use longshore_s3::creds::Credentials;
use longshore_s3::s3::{Checksum, Config, CredentialsFile, Done, Endpoint, Validate, S3};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const CONNS: u32 = 3;
pub const BUF: u32 = 16 * 1024;
pub const MS: u64 = 1_000_000;
pub const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 80)), 80);
pub const HEARTBEAT: u64 = 1 << 50;
pub const CREDS_TOKEN: u64 = 1 << 51;
pub const REREAD_MS: u64 = 200;
// Buffers: client heads 0..3, server pairs 3..9, user tx 9, (10 unused), credentials 11.
pub const TX: BufId = BufId::new(9);
pub const CREDS_BUF: BufId = BufId::new(11);
pub const fn limits(buffer_size: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(1 + CONNS * 2)
        .with_buffers(12)
        .with_buffer_size(buffer_size)
        .with_ops_max(CONNS * 3 + CONNS + 1 + 2)
}

pub const KEY_A: &str = "AKIDOLD";
pub const KEY_B: &str = "AKIDNEW";

pub fn file(access: &str) -> String {
    format!(
        "[default]\naws_access_key_id = {access}\naws_secret_access_key = secret-for-{access}\n"
    )
}

pub struct World {
    pub io: SimIo,
    pub s3: S3,
    pub server: Server,
    pub out: Completions,
    pub done: Vec<Done>,
    pub creds: FileId,
}

pub fn world_validating(seed: u64, faults: Faults, buf: u32, validate: &Validate) -> World {
    let mut io = SimIo::new(limits(buf), seed, faults);
    let mut server = Server::new(
        server::ServerConfig::builder(Sock::new(0), CONNS, 1 + CONNS, CONNS, buf, 2 << 40)
            .head_max(2048)
            .objects_max(4)
            .build()
            .unwrap(),
    );
    let _: SocketAddr = server.start(&mut io, ADDR).unwrap();
    server.allow_access_keys(&[KEY_A]);
    let creds = io.register_file("/etc/store/credentials").unwrap();
    let http = client::ClientConfig::builder(CONNS, 1, 0, buf, 1 << 40)
        .head_max(2048)
        .max_age_ns(60_000 * MS)
        .max_requests(1000)
        .request_timeout_ns(1_000 * MS)
        .zero_copy_min(4096)
        .tls_batch(1 << 20)
        .build()
        .unwrap();
    let mut s3 = S3::new(
        Config::new(
            longshore_s3::s3::Vendor::S3,
            "bucket.objects.test".into(),
            "us-test-1".into(),
            Credentials::new(KEY_A, &format!("secret-for-{KEY_A}")).unwrap(),
            http,
            Endpoint::Fixed(ADDR),
        )
        .with_bucket_in_path(None)
        .with_port(80)
        .with_credentials_file(Some(CredentialsFile::new(
            creds,
            CREDS_BUF,
            REREAD_MS * MS,
            CREDS_TOKEN,
            *validate,
        )))
        .with_requests_max(4)
        .with_attempts_max(3)
        .with_backoff_ns(50 * MS)
        .with_backoff_max_ns(400 * MS)
        .with_require_checksums(false),
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
        creds,
    }
}

impl World {
    /// Run until `n` operations have finished and virtual time has reached `until`.
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

    pub fn settle(&mut self, ms: u64) {
        let until = self.io.now_ns() + ms * MS;
        self.run(self.done.len(), until);
    }

    pub fn store(&mut self) -> Done {
        let token = self.done.len() as u64;
        self.s3
            .store(&mut self.io, token, "k", TX, 1000, Checksum::None)
            .unwrap();
        self.run(self.done.len() + 1, 0);
        *self.done.last().unwrap()
    }
}
