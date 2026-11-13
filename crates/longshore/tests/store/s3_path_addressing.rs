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
    let mut io = SimIo::new(LIMITS, seed, Faults::default());
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
    server.serve_bucket_in_path("media");
    server.start(&mut io, ADDR).unwrap();
    let mut cfg = s3_config(3, false);
    cfg.host = "objects.test".into();
    cfg.bucket_in_path = Some("media".into());
    finish_world(io, server, cfg)
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
