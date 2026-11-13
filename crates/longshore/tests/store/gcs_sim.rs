// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The Google Cloud Storage dialect in the simulator: bearer tokens,
//! CRC-32C in `x-goog-hash`, and `x-goog-if-generation-match: 0`.
//! Allocation is forbidden while operations run.

use longshore::http::client;
use longshore::http::server::{self, Behavior, Server};
use longshore::io::alloc_guard::{sealed, Guard};
use longshore::io::{BufId, Completions, FileId, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};
use longshore::store::creds::Credentials;
use longshore::store::s3::{
    Checksum, Config, CredentialsFile, Done, Endpoint, Fatal, Outcome, Vendor, S3,
};
use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

const CONNS: u32 = 3;
const BUF: u32 = 16 * 1024;
const HEAD_MAX: u32 = 4096;
const MS: u64 = 1_000_000;
const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 80), 80);
const HEARTBEAT: u64 = 1 << 50;
const REREAD_MS: u64 = 200;
// Large buffers: server pairs 0..6, user tx 6, rx 7, token file 8. Small buffers: client heads.
const TX: BufId = BufId(6);
const RX: BufId = BufId(7);
const TOKEN_BUF: BufId = BufId(8);
const LIMITS: Limits = Limits {
    sockets_max: 1 + CONNS * 2,
    buffers: 9,
    buffer_size: BUF,
    small_buffers: CONNS,
    small_buffer_size: HEAD_MAX,
    ops_max: CONNS * 3 + CONNS + 1 + 2,
};

const TOKEN_A: &str = "ya29.first-token_0123456789abcdefghijklmnopqrstuvwxyz";
const TOKEN_B: &str = "ya29.second-token_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210";

/// CRC-32C as the server's checksum hook carries it: eight base64 characters, then zeros.
fn crc32c_form(body: &[u8]) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[..8].copy_from_slice(&longshore::checksum::crc32c_header_value(
        longshore::checksum::crc32c(body),
    ));
    out
}

struct World {
    io: SimIo,
    s3: S3,
    server: Server,
    out: Completions,
    done: Vec<Done>,
    token_file: FileId,
}

fn world(seed: u64, faults: Faults, attempts_max: u32) -> World {
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
        port: 80,
        region: "auto".into(),
        credentials: Credentials::bearer(TOKEN_A).unwrap(),
        credentials_file: Some(CredentialsFile {
            file: token_file,
            buf: TOKEN_BUF,
            reread_ns: REREAD_MS * MS,
            token: 1 << 51,
        }),
        requests_max: 4,
        attempts_max,
        backoff_ns: 50 * MS,
        backoff_max_ns: 400 * MS,
        require_checksums: true,
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
        token_file,
    }
}

impl World {
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

    fn last(&mut self) -> Done {
        self.run(self.done.len() + 1, 0);
        *self.done.last().unwrap()
    }

    fn settle(&mut self, ms: u64) {
        let until = self.io.now_ns() + ms * MS;
        self.run(self.done.len(), until);
    }

    fn fill_tx(&mut self, len: u32, salt: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ salt;
        }
    }
}

#[test]
fn store_fetch_list_and_erase_with_a_bearer_token_and_crc32c() {
    let mut w = world(1, Faults::default(), 3);
    w.fill_tx(5000, 1);
    w.s3.store(&mut w.io, 1, "dir/key one.bin", TX, 5000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
    let sent = w.io.buf(TX)[..5000].to_vec();
    assert_eq!(w.server.object("/dir/key%20one.bin"), Some(&sent[..]));

    w.s3.fetch(&mut w.io, 2, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 5000,
            verified: true
        },
        "the store's CRC-32C must be checked against the bytes received"
    );
    assert_eq!(&w.io.buf(RX)[..5000], &sent[..]);
    w.s3.fetch(&mut w.io, 3, "dir/key one.bin", Some((10, 19)), RX)
        .unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: 10,
            verified: false
        }
    );

    w.s3.list(&mut w.io, 4, "dir/", None, 10, RX).unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("listing failed: {:?}", w.done.last())
    };
    let listing = longshore::store::list::Listing::parse(&w.io.buf(RX)[..len as usize]).unwrap();
    assert_eq!(listing.entries().count(), 1);

    w.s3.erase(&mut w.io, 5, "dir/key one.bin").unwrap();
    assert_eq!(w.last().outcome, Outcome::Erased);
    w.s3.fetch(&mut w.io, 6, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::NotFound);
}

/// Google answers DELETE of an absent object with 404 NoSuchKey
/// (cloud.google.com/storage/docs/xml-api/delete-object): the object is gone,
/// so the erase succeeded. Any other 404 is still a failure.
#[test]
fn an_erase_of_an_absent_object_succeeds_and_other_404s_do_not() {
    let mut w = world(4, Faults::default(), 3);
    w.s3.erase(&mut w.io, 1, "never-stored").unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Erased,
            attempts: 1
        }
    );

    // The delete is applied and its response lost: the retry finds nothing.
    w.fill_tx(100, 1);
    w.s3.store(&mut w.io, 2, "k", TX, 100, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.erase(&mut w.io, 3, "k").unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 3,
            outcome: Outcome::Erased,
            attempts: 2
        }
    );
    assert_eq!(w.server.object("/k"), None);

    // A 404 that does not name the key (a missing bucket, say) stays fatal.
    w.server.script(Behavior::StatusWithBody(404, 64));
    w.s3.erase(&mut w.io, 4, "k").unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 4,
            outcome: Outcome::Fatal(Fatal::Status(404)),
            attempts: 1
        }
    );
}

#[test]
fn conditional_store_uses_the_generation_precondition() {
    let mut w = world(2, Faults::default(), 4);
    w.fill_tx(1000, 1);
    w.s3.store_no_clobber(&mut w.io, 1, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    let first = w.io.buf(TX)[..1000].to_vec();
    w.fill_tx(1000, 2);
    w.s3.store_no_clobber(&mut w.io, 2, "k", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::AlreadyExists { after_retry: false },
            attempts: 1
        }
    );
    assert_eq!(
        w.server.object("/k"),
        Some(&first[..]),
        "the existing object must be unchanged"
    );

    // A response lost after the store applied it: the retry is refused, the
    // CRC-32C comparison recognises our own object.
    w.fill_tx(1000, 3);
    w.server.script(Behavior::ApplyThenClose);
    w.s3.store_no_clobber(&mut w.io, 3, "other", TX, 1000, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 3,
            outcome: Outcome::Stored,
            attempts: 3
        }
    );
}

#[test]
fn a_refused_token_is_fatal_and_a_new_token_is_rotated_in() {
    let mut w = world(3, Faults::default(), 3);
    w.fill_tx(100, 1);
    // The server stops accepting the token the client holds.
    w.server.allow_access_keys(&[TOKEN_B]);
    w.s3.store(&mut w.io, 1, "k", TX, 100, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 1,
            outcome: Outcome::Fatal(Fatal::Status(403)),
            attempts: 1
        }
    );

    // The token helper writes a new token; it is validated and adopted.
    w.io.set_file(w.token_file, Some(format!("{TOKEN_B}\n").as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(w.s3.credential_rotations, 1);
    w.s3.store(&mut w.io, 2, "k", TX, 100, Checksum::Compute)
        .unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Stored,
            attempts: 1
        }
    );
}

/// Bits flipped in transit: a damaged upload is refused by the server's
/// CRC-32C check and sent again, a damaged download fails its check and is
/// fetched again. Nothing wrong is ever stored or returned.
#[test]
fn corruption_in_transit_never_reaches_the_store_or_the_caller() {
    let faults = Faults {
        bit_flip: 150_000,
        ..Faults::default()
    };
    let (mut stored, mut fetched) = (0, 0);
    for seed in 0..40 {
        let mut w = world(seed, faults, 6);
        w.fill_tx(8000, seed as u8);
        let sent = w.io.buf(TX)[..8000].to_vec();
        w.s3.store(&mut w.io, 1, "k", TX, 8000, Checksum::Compute)
            .unwrap();
        if w.last().outcome == Outcome::Stored {
            stored += 1;
            assert_eq!(
                w.server.object("/k"),
                Some(&sent[..]),
                "seed {seed}: a damaged body was stored"
            );
            w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
            if let Outcome::Fetched { len, verified } = w.last().outcome {
                fetched += 1;
                assert!(
                    verified && len == 8000 && w.io.buf(RX)[..8000] == sent[..],
                    "seed {seed}: damaged bytes were returned"
                );
            }
        }
    }
    assert!(
        stored > 20 && fetched > 10,
        "the faults leave too little to check: {stored} stored, {fetched} fetched"
    );
}
