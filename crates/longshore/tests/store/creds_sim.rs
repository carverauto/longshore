// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Credential rotation in the simulator: the store rereads a credentials
//! file, validates new credentials against the server, and only then uses
//! them. Allocation is forbidden while operations run.

use longshore::http::client;
use longshore::http::server::{self, Server};
use longshore::io::alloc_guard::{sealed, Guard};
use longshore::io::{BufId, Completions, FileId, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
use longshore::store::s3::{Checksum, Config, CredentialsFile, Done, Endpoint, Outcome, S3};
use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

const CONNS: u32 = 3;
const BUF: u32 = 16 * 1024;
const MS: u64 = 1_000_000;
const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 80), 80);
const HEARTBEAT: u64 = 1 << 50;
const CREDS_TOKEN: u64 = 1 << 51;
const REREAD_MS: u64 = 200;
// Buffers: client heads 0..3, server pairs 3..9, user tx 9, (10 unused), credentials 11.
const TX: BufId = BufId(9);
const CREDS_BUF: BufId = BufId(11);
const fn limits(buffer_size: u32) -> Limits {
    Limits {
        sockets_max: 1 + CONNS * 2,
        buffers: 12,
        buffer_size,
        small_buffers: 0,
        small_buffer_size: 0,
        ops_max: CONNS * 3 + CONNS + 1 + 2,
    }
}

const KEY_A: &str = "AKIDOLD";
const KEY_B: &str = "AKIDNEW";

fn file(access: &str) -> String {
    format!(
        "[default]\naws_access_key_id = {access}\naws_secret_access_key = secret-for-{access}\n"
    )
}

struct World {
    io: SimIo,
    s3: S3,
    server: Server,
    out: Completions,
    done: Vec<Done>,
    creds: FileId,
}

fn world(seed: u64, faults: Faults) -> World {
    world_with_buffers(seed, faults, BUF)
}

fn world_with_buffers(seed: u64, faults: Faults, buf: u32) -> World {
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
        port: 80,
        region: "us-test-1".into(),
        credentials: Credentials::new(KEY_A, &format!("secret-for-{KEY_A}")).unwrap(),
        credentials_file: Some(CredentialsFile {
            file: creds,
            buf: CREDS_BUF,
            reread_ns: REREAD_MS * MS,
            token: CREDS_TOKEN,
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
    fn run(&mut self, n: usize, until: u64) {
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

    fn settle(&mut self, ms: u64) {
        let until = self.io.now_ns() + ms * MS;
        self.run(self.done.len(), until);
    }

    fn store(&mut self) -> Done {
        let token = self.done.len() as u64;
        self.s3
            .store(&mut self.io, token, "k", TX, 1000, Checksum::None)
            .unwrap();
        self.run(self.done.len() + 1, 0);
        *self.done.last().unwrap()
    }
}

#[test]
fn new_credentials_are_adopted_only_after_the_store_accepts_them() {
    let mut w = world(1, Faults::default());
    // No file yet: a failed read, and the configured credentials stay in use.
    w.settle(REREAD_MS + 50);
    assert_eq!(
        (
            w.s3.access_key(),
            w.s3.credential_rotations,
            w.s3.credential_failures
        ),
        (KEY_A, 0, 1)
    );
    assert_eq!(w.store().outcome, Outcome::Stored);

    // The file now names a key the server does not accept yet.
    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(
        (w.s3.access_key(), w.s3.credential_rotations),
        (KEY_A, 0),
        "refused credentials must not be adopted"
    );
    assert!(w.s3.credential_failures >= 2);
    assert_eq!(
        w.store().outcome,
        Outcome::Stored,
        "requests keep working on the old credentials"
    );

    // The operator enables the new key alongside the old one.
    w.server.allow_access_keys(&[KEY_A, KEY_B]);
    w.settle(REREAD_MS + 50);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));

    // The old key is revoked; requests are signed with the new one.
    w.server.allow_access_keys(&[KEY_B]);
    assert_eq!(w.store().outcome, Outcome::Stored);
    assert_eq!(w.store().attempts, 1);

    // An unchanged file causes no further validation requests.
    let (requests, failures) = (w.server.requests, w.s3.credential_failures);
    w.settle(5 * REREAD_MS);
    assert_eq!(
        (
            w.server.requests,
            w.s3.credential_rotations,
            w.s3.credential_failures
        ),
        (requests, 1, failures),
        "an unchanged file must not trigger validation"
    );

    // A half-written or garbage file is counted and ignored.
    w.io.set_file(
        w.creds,
        Some(b"[default]\naws_access_key_id = AKIDHALF\naws_secret_acc"),
    );
    w.settle(REREAD_MS + 50);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));
    assert!(w.s3.credential_failures > failures);
    assert_eq!(w.store().outcome, Outcome::Stored);
}

/// A file longer than one read takes cannot be known to be complete, even
/// when the part read holds a whole key pair, so it is refused. The read is
/// bounded by the client's buffer here, which is smaller than the file limit.
#[test]
fn a_file_longer_than_one_read_is_refused() {
    let buf = 4096;
    let mut w = world_with_buffers(1, Faults::default(), buf);
    w.server.allow_access_keys(&[KEY_A, KEY_B]);
    let mut text = file(KEY_B);
    while text.len() <= buf as usize {
        text.push_str("# padding that pushes the file past one read\n");
    }
    w.io.set_file(w.creds, Some(text.as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(
        (w.s3.access_key(), w.s3.credential_rotations),
        (KEY_A, 0),
        "a file longer than one read must not be adopted"
    );
    assert!(w.s3.credential_failures >= 1);

    // The same key pair in a file that fits is adopted.
    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));
}

/// The whole rotation, with requests running throughout and network faults
/// injected: add the new key, rotate the file, revoke the old key. No
/// request may fail.
#[test]
fn rotation_under_load_fails_no_request() {
    let faults = Faults {
        short_send: 200_000,
        partial_recv: 200_000,
        delay: 200_000,
        ..Faults::default()
    };
    for seed in 0..20 {
        let mut w = world(seed, faults);
        let mut stage = 0;
        for i in 0..120 {
            let d = w.store();
            assert_eq!(
                d.outcome,
                Outcome::Stored,
                "seed {seed}, request {i}, stage {stage}: {d:?}"
            );
            match (stage, i) {
                (0, 20) => {
                    w.server.allow_access_keys(&[KEY_A, KEY_B]);
                    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
                    stage = 1;
                }
                // Revoke the old key only once the client has moved off it.
                (1, _) if w.s3.credential_rotations == 1 => {
                    w.server.allow_access_keys(&[KEY_B]);
                    stage = 2;
                }
                _ => {}
            }
            w.settle(10);
        }
        assert_eq!(
            (stage, w.s3.access_key(), w.s3.credential_rotations),
            (2, KEY_B, 1),
            "seed {seed}: rotation did not complete"
        );
    }
}

/// Temporary keys work only with their session token: the store sends it
/// with every request, and picks up a new token when the file changes.
#[test]
fn temporary_keys_send_their_session_token_and_rotate_with_it() {
    let mut w = world(7, Faults::default());
    // The server now insists on a token. The configured keys have none.
    w.server.require_session_token("token-one");
    assert_eq!(
        w.store().outcome,
        Outcome::Fatal(longshore::store::s3::Fatal::Status(403))
    );

    // The file supplies the same keys with a token: a change, so it is validated and adopted.
    let with_token = |token: &str| {
        format!("[default]\naws_access_key_id = {KEY_A}\naws_secret_access_key = secret-for-{KEY_A}\naws_session_token = {token}\n")
    };
    w.io.set_file(w.creds, Some(with_token("token-one").as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(w.s3.credential_rotations, 1);
    assert_eq!(w.store().outcome, Outcome::Stored);

    // The token is replaced, as it is every few hours in practice.
    w.server.require_session_token("token-two");
    w.io.set_file(w.creds, Some(with_token("token-two").as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(w.s3.credential_rotations, 2);
    assert_eq!(w.store().outcome, Outcome::Stored);
}
