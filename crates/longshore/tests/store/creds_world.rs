// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A store that rereads a credentials file, beside the in-tree server on
//! one simulated `Io`, shared by the credential tests.

#![allow(dead_code)]

use longshore::http::client;
use longshore::http::server::{self, Server};
use longshore::io::alloc_guard::sealed;
use longshore::io::{BufId, Completions, FileId, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
use longshore::store::s3::{
    Checksum, Config, CredentialsFile, Done, Endpoint, KeyPrefix, Validate, S3,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const CONNS: u32 = 3;
pub const BUF: u32 = 16 * 1024;
pub const MS: u64 = 1_000_000;
pub const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 80)), 80);
pub const HEARTBEAT: u64 = 1 << 50;
pub const CREDS_TOKEN: u64 = 1 << 51;
pub const REREAD_MS: u64 = 200;
// Buffers: client heads 0..3, server pairs 3..9, user tx 9, (10 unused), credentials 11.
pub const TX: BufId = BufId(9);
pub const CREDS_BUF: BufId = BufId(11);
pub const fn limits(buffer_size: u32) -> Limits {
    Limits {
        sockets_max: 1 + CONNS * 2,
        buffers: 12,
        buffer_size,
        small_buffers: 0,
        small_buffer_size: 0,
        ops_max: CONNS * 3 + CONNS + 1 + 2,
    }
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

pub fn world(seed: u64, faults: Faults) -> World {
    world_with_buffers(seed, faults, BUF)
}

pub fn world_with_buffers(seed: u64, faults: Faults, buf: u32) -> World {
    world_validating(seed, faults, buf, Validate::List(KeyPrefix::EMPTY))
}

pub fn world_validating(seed: u64, faults: Faults, buf: u32, validate: Validate) -> World {
    let mut io = SimIo::new(limits(buf), seed, faults);
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: CONNS,
        head_max: 2048,
        buffer_size: buf,
        objects_max: 4,
        token_base: 2 << 40,
        checksum: None,
    });
    server.start(&mut io, ADDR).unwrap();
    server.allow_access_keys(&[KEY_A]);
    let creds = io.register_file("/etc/store/credentials").unwrap();
    let http = client::Config {
        connections_max: CONNS,
        first_sock: 1,
        first_buf: 0,
        head_max: 2048,
        buffer_size: buf,
        max_age_ns: 60_000 * MS,
        max_requests: 1000,
        request_timeout_ns: 1_000 * MS,
        zero_copy_min: 4096,
        token_base: 1 << 40,
        tls_batch: 1 << 20,
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    };
    let mut s3 = S3::new(Config {
        vendor: longshore::store::s3::Vendor::S3,
        host: "bucket.objects.test".into(),
        bucket_in_path: None,
        port: 80,
        region: "us-test-1".into(),
        credentials: Credentials::new(KEY_A, &format!("secret-for-{KEY_A}")).unwrap(),
        credentials_file: Some(CredentialsFile {
            file: creds,
            buf: CREDS_BUF,
            reread_ns: REREAD_MS * MS,
            token: CREDS_TOKEN,
            validate,
        }),
        requests_max: 4,
        attempts_max: 3,
        backoff_ns: 50 * MS,
        backoff_max_ns: 400 * MS,
        require_checksums: false,
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
