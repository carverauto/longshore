// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Handshake wrappers: client against server in memory, with the bytes
//! delivered in small pieces, then the record layer in both directions.

use osc_io::alloc_guard::{sealed, Guard};
use osc_tls::handshake::{ClientSetup, Handshake, ServerSetup, TlsError};
use osc_tls::record::{Direction, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE, OVERHEAD};

#[global_allocator]
static ALLOC: Guard = Guard;

const NAME: &str = "objects.test";

struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

fn identity(name: &str) -> Identity {
    let k = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    Identity {
        cert: k.cert.der().to_vec(),
        key: k.signing_key.serialize_der(),
    }
}

/// Run both sides to completion, moving at most `step` bytes per hop, and
/// return any bytes the peer sent after its handshake finished.
fn run(client: &mut Handshake, server: &mut Handshake, step: usize) -> Result<Vec<u8>, TlsError> {
    let (mut to_server, mut to_client) = (Vec::new(), Vec::new());
    // How much of each queue has "arrived" so far: grows by `step` per turn,
    // so records straddle deliveries.
    let (mut at_server, mut at_client) = (0usize, 0usize);
    let mut buf = vec![0u8; 65536];
    for _ in 0..200_000 {
        let n = client.write(&mut buf);
        to_server.extend_from_slice(&buf[..n]);
        let n = server.write(&mut buf);
        to_client.extend_from_slice(&buf[..n]);
        at_server = (at_server + step).min(to_server.len());
        at_client = (at_client + step).min(to_client.len());
        let used = server.read(&to_server[..at_server])?;
        to_server.drain(..used);
        at_server -= used;
        let used = client.read(&to_client[..at_client])?;
        to_client.drain(..used);
        at_client -= used;
        if client.is_done() && server.is_done() && to_server.is_empty() {
            return Ok(to_client);
        }
    }
    panic!("handshake did not finish");
}

fn established(step: usize) -> (Direction, Direction, Direction, Direction, Vec<u8>) {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let (mut c, mut s) = (
        Handshake::client(&cs, NAME).unwrap(),
        Handshake::server(&ss).unwrap(),
    );
    let leftover = run(&mut c, &mut s, step).unwrap();
    let (mut ctx, mut crx, mut stx, mut srx) = (
        Direction::new(),
        Direction::new(),
        Direction::new(),
        Direction::new(),
    );
    c.finish(&mut ctx, &mut crx).unwrap();
    s.finish(&mut stx, &mut srx).unwrap();
    (ctx, crx, stx, srx, leftover)
}

#[test]
fn handshake_completes_and_the_record_layer_takes_over_in_both_directions() {
    for step in [1usize, 7, 100, 8192] {
        let (mut ctx, mut crx, mut stx, mut srx, leftover) = established(step);
        // Anything the server sent after its handshake (session tickets) must
        // open with the client's receive keys, as handshake-type records.
        let mut rest = &leftover[..];
        let mut out = vec![0u8; 20_000];
        while !rest.is_empty() {
            let len = osc_tls::record::record_len(rest)
                .unwrap()
                .expect("whole record");
            let (ctype, _) = crx
                .open(&rest[..len], &mut out)
                .unwrap_or_else(|e| panic!("step {step}: leftover record: {e:?}"));
            assert_eq!(ctype, CONTENT_HANDSHAKE, "post-handshake message");
            rest = &rest[len..];
        }
        let mut rec = vec![0u8; 1000 + OVERHEAD];
        for (round, msg) in [&b"PUT /k HTTP/1.1\r\n\r\n"[..], &[0x5a; 1000]]
            .into_iter()
            .enumerate()
        {
            let n = ctx.seal(CONTENT_APPLICATION_DATA, msg, &mut rec).unwrap();
            assert_eq!(
                srx.open(&rec[..n], &mut out),
                Ok((CONTENT_APPLICATION_DATA, msg.len())),
                "step {step} round {round} client to server"
            );
            assert_eq!(&out[..msg.len()], msg);
            let n = stx.seal(CONTENT_APPLICATION_DATA, msg, &mut rec).unwrap();
            assert_eq!(
                crx.open(&rec[..n], &mut out),
                Ok((CONTENT_APPLICATION_DATA, msg.len())),
                "step {step} round {round} server to client"
            );
        }
    }
}

#[test]
fn an_untrusted_certificate_is_refused() {
    let id = identity(NAME);
    let other = identity(NAME);
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    // The client trusts a different certificate with the same name.
    let cs = ClientSetup::with_roots(&[&other.cert]).unwrap();
    let (mut c, mut s) = (
        Handshake::client(&cs, NAME).unwrap(),
        Handshake::server(&ss).unwrap(),
    );
    assert_eq!(run(&mut c, &mut s, 4096), Err(TlsError::BadCertificate));
    // And the public root set does not include a self-signed test certificate.
    let cs = ClientSetup::with_public_roots();
    let (mut c, mut s) = (
        Handshake::client(&cs, NAME).unwrap(),
        Handshake::server(&ss).unwrap(),
    );
    assert_eq!(run(&mut c, &mut s, 4096), Err(TlsError::BadCertificate));
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let id = identity("other.test");
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let (mut c, mut s) = (
        Handshake::client(&cs, NAME).unwrap(),
        Handshake::server(&ss).unwrap(),
    );
    assert_eq!(run(&mut c, &mut s, 4096), Err(TlsError::BadCertificate));
}

#[test]
fn finish_before_completion_and_garbage_input_are_errors() {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let c = Handshake::client(&cs, NAME).unwrap();
    assert_eq!(
        c.finish(&mut Direction::new(), &mut Direction::new()),
        Err(TlsError::NotDone)
    );
    let mut c = Handshake::client(&cs, NAME).unwrap();
    assert_eq!(
        c.read(b"HTTP/1.1 400 Bad Request\r\n\r\n"),
        Err(TlsError::BadRecord),
        "a plain-HTTP answer on a TLS port"
    );
    assert_eq!(
        Handshake::client(&cs, "not a hostname").err(),
        Some(TlsError::Setup)
    );
}

/// The allocation exception covers the handshake and nothing after it.
#[test]
fn the_handshake_runs_inside_a_sealed_region_and_records_stay_allocation_free() {
    let id = identity(NAME);
    let cs = ClientSetup::with_roots(&[&id.cert]).unwrap();
    let ss = ServerSetup::new(&[&id.cert], &id.key).unwrap();
    let (mut ctx, mut crx, mut stx, mut srx) = (
        Direction::new(),
        Direction::new(),
        Direction::new(),
        Direction::new(),
    );
    let (mut a, mut b) = (vec![0u8; 16_384], vec![0u8; 16_384]);
    let mut out = vec![0u8; 4096];
    let ok = sealed(|| {
        let (mut c, mut s) = (
            Handshake::client(&cs, NAME).unwrap(),
            Handshake::server(&ss).unwrap(),
        );
        // Fixed buffers only: this loop itself must not allocate.
        let (mut an, mut bn) = (0usize, 0usize);
        for _ in 0..1000 {
            an += c.write(&mut a[an..]);
            bn += s.write(&mut b[bn..]);
            let used = s.read(&a[..an]).unwrap();
            a.copy_within(used..an, 0);
            an -= used;
            let used = c.read(&b[..bn]).unwrap();
            b.copy_within(used..bn, 0);
            bn -= used;
            if c.is_done() && s.is_done() && an == 0 {
                break;
            }
        }
        c.finish(&mut ctx, &mut crx).unwrap();
        s.finish(&mut stx, &mut srx).unwrap();
        let n = ctx
            .seal(
                CONTENT_APPLICATION_DATA,
                b"sealed and still working",
                &mut a,
            )
            .unwrap();
        srx.open(&a[..n], &mut out) == Ok((CONTENT_APPLICATION_DATA, 24))
    });
    assert!(ok);
}

/// A certificate large enough that its handshake record exceeds what rustls
/// reads in one call. Real certificate chains are this big; the first
/// version of the wrapper lost the tail of such a record and hung.
#[test]
fn a_handshake_record_larger_than_one_read_is_fed_completely() {
    let names: Vec<String> = std::iter::once(NAME.to_string())
        .chain((0..400).map(|i| format!("alternative-name-number-{i}.objects.test")))
        .collect();
    let k = rcgen::generate_simple_self_signed(names).unwrap();
    let (cert, key) = (k.cert.der().to_vec(), k.signing_key.serialize_der());
    assert!(
        cert.len() > 8192,
        "certificate is only {} bytes",
        cert.len()
    );
    let cs = ClientSetup::with_roots(&[&cert]).unwrap();
    let ss = ServerSetup::new(&[&cert], &key).unwrap();
    for step in [1usize, 1000, 4096, 4097, 100_000] {
        let (mut c, mut s) = (
            Handshake::client(&cs, NAME).unwrap(),
            Handshake::server(&ss).unwrap(),
        );
        run(&mut c, &mut s, step).unwrap_or_else(|e| panic!("step {step}: {e:?}"));
        let (mut ctx, mut crx, mut stx, mut srx) = (
            Direction::new(),
            Direction::new(),
            Direction::new(),
            Direction::new(),
        );
        c.finish(&mut ctx, &mut crx).unwrap();
        s.finish(&mut stx, &mut srx).unwrap();
        let (mut rec, mut out) = (vec![0u8; 100], vec![0u8; 100]);
        let n = ctx
            .seal(CONTENT_APPLICATION_DATA, b"after a large flight", &mut rec)
            .unwrap();
        assert_eq!(
            srx.open(&rec[..n], &mut out),
            Ok((CONTENT_APPLICATION_DATA, 20)),
            "step {step}"
        );
    }
}
