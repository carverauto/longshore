// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The S3 store with the bucket addressed in the path, against a server that
//! expects it there.

mod common;
use common::*;
use longshore::store::erase::write_manifest;
use longshore::store::presign::Access;

const LEN: u32 = 300;

/// A store configured for `/media/key` on the endpoint's own host name, and
/// a server that serves the bucket `media` from its path.
fn world_in_path(seed: u64) -> World {
    world_for_bucket(seed, "media")
}

/// A store configured for `/bucket/key`, and a server that serves `bucket`.
fn world_for_bucket(seed: u64, bucket: &'static str) -> World {
    let (io, server) = path_server(SimIo::new(LIMITS, seed, Faults::default()), bucket);
    finish_world(io, server, store_config(bucket, false))
}

/// A server listening on `ADDR` that serves `bucket` from its path.
fn path_server(mut io: SimIo, bucket: &'static str) -> (SimIo, Server) {
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 0,
        head_max: 2048,
        buffer_size: BUF,
        objects_max: 8,
        token_base: 2 << 40,
        checksum: None,
    });
    server.serve_bucket_in_path(bucket);
    server.start(&mut io, ADDR).unwrap();
    (io, server)
}

/// A store addressing `bucket` in the path. A `second` store takes the
/// sockets and head buffers after the first's, so two can share one world.
fn store_config(bucket: &str, second: bool) -> Config {
    let mut cfg = s3_config(3, false);
    cfg.host = "objects.test".into();
    cfg.bucket_in_path = Some(bucket.into());
    if second {
        cfg.http.first_sock = 1 + 2 * CONNS;
        cfg.http.first_buf = LIMITS.buffers + CONNS;
        cfg.http.token_base = 3 << 40;
    }
    cfg
}

/// Run `w` and a second store `other` until `n` operations have finished in total.
fn run_both(w: &mut World, other: &mut S3, n: usize) {
    let World {
        io,
        s3,
        server,
        out,
        done,
    } = w;
    while done.len() < n {
        io.tick(true, out).expect("simulation deadlock");
        while let Some(c) = out.pop() {
            if c.token.0 == HEARTBEAT {
                s3.tick(io);
                other.tick(io);
                io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                    .unwrap();
            } else if !s3.on_completion(io, c) && !other.on_completion(io, c) {
                assert!(server.on_completion(io, c), "unrouted completion {c:?}");
            }
        }
        while let Some(d) = s3.poll().or_else(|| other.poll()) {
            done.push(d);
        }
    }
}

fn put(w: &mut World, token: u64, key: &str) {
    w.fill_tx(LEN, token as u8);
    w.s3.store(&mut w.io, token, key, TX, LEN, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored, "{key}");
}

#[test]
fn objects_are_stored_fetched_and_erased_under_the_bucket() {
    let mut w = world_in_path(1);
    put(&mut w, 1, "dir/key one.bin");
    // The server saw "/media/dir/key%20one.bin" and holds the key alone.
    assert_eq!(w.server.object_len("/dir/key%20one.bin"), Some(LEN));
    let sent = w.io.buf(TX)[..LEN as usize].to_vec();

    w.s3.fetch(&mut w.io, 2, "dir/key one.bin", Some((10, 19)), RX)
        .unwrap();
    assert!(matches!(w.last().outcome, Outcome::Fetched { len: 10, .. }));
    assert!(w.io.buf(RX)[..10] == sent[10..20]);

    w.s3.store_no_clobber(&mut w.io, 3, "dir/key one.bin", TX, LEN, Checksum::None)
        .unwrap();
    assert!(
        matches!(w.last().outcome, Outcome::AlreadyExists { .. }),
        "{:?}",
        w.done.last()
    );

    w.s3.erase(&mut w.io, 4, "dir/key one.bin").unwrap();
    assert_eq!(w.last().outcome, Outcome::Erased);
    w.s3.fetch(&mut w.io, 5, "dir/key one.bin", None, RX)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::NotFound);
}

#[test]
fn a_listing_names_keys_without_the_bucket() {
    let mut w = world_in_path(2);
    for (i, key) in ["logs/a", "logs/b", "other/c"].into_iter().enumerate() {
        put(&mut w, i as u64, key);
    }
    w.s3.list(&mut w.io, 10, "logs/", None, 100, RX).unwrap();
    let Outcome::Listed { len } = w.last().outcome else {
        panic!("list failed: {:?}", w.done.last())
    };
    let xml = w.io.buf(RX)[..len as usize].to_vec();
    let listing = Listing::parse(&xml).unwrap();
    let keys: Vec<String> = listing
        .entries()
        .map(|e| String::from_utf8(e.unwrap().key_raw.to_vec()).unwrap())
        .collect();
    assert_eq!(keys, ["logs/a", "logs/b"]);
}

#[test]
fn a_copy_names_its_source_by_the_bucket_in_the_path() {
    let mut w = world_in_path(3);
    put(&mut w, 1, "a/src");
    w.s3.copy(&mut w.io, 2, "a/src", "b/dst").unwrap();
    assert_eq!(w.last().outcome, Outcome::Copied);
    assert_eq!(w.server.object_len("/b/dst"), Some(LEN));
}

#[test]
fn a_batch_erase_goes_to_the_bucket() {
    let mut w = world_in_path(4);
    put(&mut w, 1, "x/1");
    put(&mut w, 2, "x/2");
    let n = write_manifest(w.io.buf(TX), &["x/1", "x/2"]).unwrap();
    w.s3.erase_many(&mut w.io, 3, TX, n as u32, RX).unwrap();
    assert!(
        matches!(w.last().outcome, Outcome::ErasedMany { .. }),
        "{:?}",
        w.done.last()
    );
    assert_eq!(w.server.object_len("/x/1"), None);
    assert_eq!(w.server.object_len("/x/2"), None);
}

#[test]
fn a_presigned_url_has_the_endpoint_as_host_and_the_bucket_in_its_path() {
    let mut w = world_in_path(5);
    let mut out = [0u8; 1024];
    let n =
        w.s3.presign(&mut w.io, &mut out, Access::Fetch, "dir/a b", 3600)
            .unwrap();
    let url = core::str::from_utf8(&out[..n]).unwrap();
    assert!(
        url.starts_with("http://objects.test/media/dir/a%20b?X-Amz-Algorithm="),
        "{url}"
    );
}

#[test]
#[should_panic(expected = "not a bucket name")]
fn a_bucket_name_that_could_not_be_one_is_refused_when_the_store_is_built() {
    let mut cfg = s3_config(3, false);
    cfg.bucket_in_path = Some("Bad/Name".into());
    S3::new(cfg);
}

#[test]
fn unreserved_characters_reach_the_path_and_the_url_unencoded() {
    // SigV4 URI-encoding leaves A-Z a-z 0-9 - . _ ~ as they are, in the
    // bucket and in the key.
    let mut w = world_for_bucket(6, "my.bucket-1");
    put(&mut w, 1, "dir/a.b~c_d.txt");
    assert_eq!(w.server.object_len("/dir/a.b~c_d.txt"), Some(LEN));
    w.s3.fetch(&mut w.io, 2, "dir/a.b~c_d.txt", None, RX)
        .unwrap();
    assert!(matches!(
        w.last().outcome,
        Outcome::Fetched { len: LEN, .. }
    ));

    let mut out = [0u8; 1024];
    let n =
        w.s3.presign(&mut w.io, &mut out, Access::Fetch, "dir/a.b~c_d.txt", 3600)
            .unwrap();
    let url = core::str::from_utf8(&out[..n]).unwrap();
    assert!(
        url.starts_with("http://objects.test/my.bucket-1/dir/a.b~c_d.txt?"),
        "{url}"
    );
}

#[test]
fn a_store_for_another_bucket_does_not_reach_the_served_one() {
    let limits = Limits {
        sockets_max: 1 + CONNS * 3,
        small_buffers: CONNS * 2,
        ops_max: (CONNS * 3 + 2) * 2,
        ..LIMITS
    };
    let (io, server) = path_server(SimIo::new(limits, 7, Faults::default()), "media");
    let mut w = finish_world(io, server, store_config("media-2", false));
    let mut media = S3::new(store_config("media", true));
    media.start(&mut w.io);

    w.fill_tx(LEN, 1);
    media
        .store(&mut w.io, 1, "shared", TX, LEN, Checksum::None)
        .unwrap();
    run_both(&mut w, &mut media, 1);
    assert_eq!(w.done[0].outcome, Outcome::Stored);
    assert_eq!(w.server.object_len("/shared"), Some(LEN));

    // The other bucket's store keeps its own prefix: the server, which strips
    // only "/media/", holds the object under "/media-2/...".
    w.s3.store(&mut w.io, 2, "mine", TX, LEN, Checksum::None)
        .unwrap();
    run_both(&mut w, &mut media, 2);
    assert_eq!(w.done[1].outcome, Outcome::Stored);
    assert_eq!(w.server.object_len("/media-2/mine"), Some(LEN));
    assert_eq!(w.server.object_len("/mine"), None);

    w.s3.fetch(&mut w.io, 3, "shared", None, RX).unwrap();
    run_both(&mut w, &mut media, 3);
    assert_eq!(w.done[2].outcome, Outcome::NotFound);
}
