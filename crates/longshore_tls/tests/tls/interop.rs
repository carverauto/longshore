// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The handshake wrapper against a peer that is not this crate: rustls on
//! its own, which keeps protecting records after the handshake. Both ends
//! of the other handshake tests take their keys through `Handshake::finish`,
//! so a mistake made the same way on both ends would cancel out there. Here
//! it cannot: the keys, their direction and their starting sequence numbers
//! must be the ones rustls itself goes on to use.

use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup};
use longshore_tls::record::{
    record_len, Direction, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE, OVERHEAD,
};
use rustls::crypto::aws_lc_rs::default_provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{CipherSuite, ClientConfig, ClientConnection, Connection, RootCertStore};
use rustls::{ServerConfig, ServerConnection};
use std::io::{Read, Write};
use std::sync::Arc;

const NAME: &str = "objects.test";

fn identity() -> (Vec<u8>, Vec<u8>) {
    let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
    (k.cert.der().to_vec(), k.signing_key.serialize_der())
}

/// A rustls server that offers `suite` and nothing else.
fn rustls_server(cert: &[u8], key: &[u8], suite: CipherSuite) -> Connection {
    let mut provider = default_provider();
    provider.cipher_suites.retain(|s| s.suite() == suite);
    assert_eq!(provider.cipher_suites.len(), 1);
    let config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
        )
        .unwrap();
    ServerConnection::new(Arc::new(config)).unwrap().into()
}

/// A rustls client with its own defaults, trusting `cert`.
fn rustls_client(cert: &[u8]) -> Connection {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(cert.to_vec())).unwrap();
    let config = ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(NAME).unwrap();
    ClientConnection::new(Arc::new(config), name)
        .unwrap()
        .into()
}

/// Give `bytes` to rustls, all of them.
fn feed(peer: &mut Connection, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        peer.read_tls(&mut bytes).unwrap();
        peer.process_new_packets().unwrap();
    }
}

/// Everything rustls has to send.
fn drain(peer: &mut Connection, out: &mut Vec<u8>) {
    while peer.wants_write() {
        peer.write_tls(out).unwrap();
    }
}

/// Run the handshake to completion. Returns what rustls sent that the
/// wrapper left for the record layer.
fn handshake(ours: &mut Handshake, peer: &mut Connection) -> Vec<u8> {
    let mut to_us = Vec::new();
    let mut buf = vec![0u8; 65536];
    for _ in 0..100 {
        let n = ours.write(&mut buf);
        feed(peer, &buf[..n]);
        drain(peer, &mut to_us);
        let used = ours.read(&to_us).unwrap();
        to_us.drain(..used);
        if ours.is_done() && !peer.is_handshaking() && !peer.wants_write() {
            return to_us;
        }
    }
    panic!("handshake did not finish");
}

/// Open every record in `bytes` with `rx`: (content type, content) of each.
fn open_all(rx: &mut Direction, mut bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut opened = Vec::new();
    let mut out = vec![0u8; 20_000];
    while !bytes.is_empty() {
        let len = record_len(bytes).unwrap().expect("whole record");
        let (ctype, n) = rx
            .open(&bytes[..len], &mut out)
            .unwrap_or_else(|e| panic!("record {} from rustls: {e:?}", opened.len()));
        opened.push((ctype, out[..n].to_vec()));
        bytes = &bytes[len..];
    }
    opened
}

fn suite_of(peer: &Connection) -> CipherSuite {
    peer.negotiated_cipher_suite().unwrap().suite()
}

/// Our client against a rustls server, for each suite: what the client
/// seals rustls reads, and what rustls writes the client opens, session
/// tickets first if the server sent any.
#[test]
fn a_client_exchanges_records_with_a_rustls_server_under_each_suite() {
    let (cert, key) = identity();
    let setup = ClientSetup::with_roots(&[&cert]).unwrap();
    for suite in [
        CipherSuite::TLS13_AES_128_GCM_SHA256,
        CipherSuite::TLS13_AES_256_GCM_SHA384,
    ] {
        let mut server = rustls_server(&cert, &key, suite);
        let mut client = Handshake::client(&setup, NAME).unwrap();
        let mut from_server = handshake(&mut client, &mut server);
        assert_eq!(suite_of(&server), suite);
        let (mut tx, mut rx) = (Direction::new(), Direction::new());
        client.finish(&mut tx, &mut rx).unwrap();
        assert_eq!((tx.sequence(), rx.sequence()), (0, 0));

        let mut rec = vec![0u8; 64];
        let mut got = [0u8; 64];
        for msg in [&b"ping"[..], b"ping again"] {
            let n = tx.seal(CONTENT_APPLICATION_DATA, msg, &mut rec).unwrap();
            assert_eq!(n, msg.len() + OVERHEAD);
            feed(&mut server, &rec[..n]);
            let n = server.reader().read(&mut got).unwrap();
            assert_eq!(&got[..n], msg, "{suite:?}: client to rustls");
        }

        server.writer().write_all(b"pong").unwrap();
        drain(&mut server, &mut from_server);
        let opened = open_all(&mut rx, &from_server);
        let (last, before) = opened.split_last().unwrap();
        assert_eq!(
            last,
            &(CONTENT_APPLICATION_DATA, b"pong".to_vec()),
            "{suite:?}: rustls to client"
        );
        assert!(
            before.iter().all(|(t, _)| *t == CONTENT_HANDSHAKE),
            "{suite:?}: only post-handshake messages precede the data"
        );
        assert_eq!(rx.sequence(), opened.len() as u64);
    }
}

/// Our server against a rustls client. The server sends session tickets
/// under its traffic keys before the handshake is handed over, so its
/// sending direction must start past them: at a sequence number other than
/// zero, which only the handover can know.
#[test]
fn a_server_hands_over_past_the_tickets_it_sent_to_a_rustls_client() {
    let (cert, key) = identity();
    let setup = ServerSetup::new(&[&cert], &key).unwrap();
    let mut client = rustls_client(&cert);
    let mut server = Handshake::server(&setup).unwrap();
    let from_client = handshake(&mut server, &mut client);
    assert!(from_client.is_empty(), "the client has sent no data yet");
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    server.finish(&mut tx, &mut rx).unwrap();
    assert!(
        tx.sequence() > 0,
        "the server's tickets were sent under these keys"
    );
    assert_eq!(rx.sequence(), 0);

    let mut rec = vec![0u8; 64];
    let n = tx
        .seal(CONTENT_APPLICATION_DATA, b"pong", &mut rec)
        .unwrap();
    feed(&mut client, &rec[..n]);
    let mut got = [0u8; 64];
    let n = client.reader().read(&mut got).unwrap();
    assert_eq!(&got[..n], b"pong", "server to rustls");

    client.writer().write_all(b"ping").unwrap();
    let mut from_client = Vec::new();
    drain(&mut client, &mut from_client);
    assert_eq!(
        open_all(&mut rx, &from_client),
        [(CONTENT_APPLICATION_DATA, b"ping".to_vec())],
        "rustls to server"
    );
}

/// The cipher suites in a ClientHello, in the order offered.
fn offered(hello: &[u8]) -> Vec<u16> {
    // Record header, handshake header, version, random.
    let at = 5 + 4 + 2 + 32;
    let at = at + 1 + hello[at] as usize; // session id
    let len = u16::from_be_bytes([hello[at], hello[at + 1]]) as usize;
    hello[at + 2..at + 2 + len]
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect()
}

/// Only the AES-GCM suites are offered, AES-128 first, and only AES-128
/// when that is asked for. Nothing else could be handed to the record layer.
#[test]
fn the_client_offers_the_aes_gcm_suites_and_nothing_else() {
    const AES_128: u16 = 0x1301;
    const AES_256: u16 = 0x1302;
    // Not a suite: a signal rustls adds to every ClientHello.
    const RENEGOTIATION_INFO: u16 = 0x00ff;
    let suites = |setup: &ClientSetup| {
        let mut buf = [0u8; 4096];
        let n = Handshake::client(setup, NAME).unwrap().write(&mut buf);
        assert_eq!(buf[..6], [22, 3, 1, buf[3], buf[4], 1], "a ClientHello");
        let mut all = offered(&buf[..n]);
        all.retain(|s| *s != RENEGOTIATION_INFO);
        all
    };
    assert_eq!(
        suites(&ClientSetup::with_public_roots()),
        [AES_128, AES_256]
    );
    assert_eq!(suites(&ClientSetup::with_public_roots_aes128()), [AES_128]);
    let (cert, _) = identity();
    assert_eq!(
        suites(&ClientSetup::with_roots(&[&cert]).unwrap()),
        [AES_128, AES_256]
    );
}
