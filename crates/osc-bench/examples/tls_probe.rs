// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Can an existing TLS library do only the handshake, and our record layer
//! do everything after it?
//!
//! Handshake with rustls over a blocking socket, extract the negotiated
//! traffic keys, then speak HTTP through `osc_tls::record` alone.
//!
//!   cargo run -p osc-bench --example tls_probe -- HOST [PATH]

use osc_tls::record::{
    record_len, Direction, Suite, CONTENT_ALERT, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE,
    HEADER_LEN, OVERHEAD, TAG_LEN,
};
use rustls::crypto::aws_lc_rs as provider;
use rustls::{ClientConfig, ClientConnection, ConnectionTrafficSecrets, RootCertStore};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

fn keys(s: &ConnectionTrafficSecrets) -> (Suite, Vec<u8>, [u8; 12]) {
    match s {
        ConnectionTrafficSecrets::Aes128Gcm { key, iv } => (
            Suite::Aes128Gcm,
            key.as_ref().to_vec(),
            iv.as_ref().try_into().unwrap(),
        ),
        ConnectionTrafficSecrets::Aes256Gcm { key, iv } => (
            Suite::Aes256Gcm,
            key.as_ref().to_vec(),
            iv.as_ref().try_into().unwrap(),
        ),
        _ => panic!("only AES-GCM suites are offered"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("usage: tls_probe HOST [PATH]");
    let path = args.next().unwrap_or_else(|| "/".into());

    // TLS 1.3 only, AES-GCM suites only.
    let mut p = provider::default_provider();
    p.cipher_suites.retain(|s| {
        matches!(
            s.suite(),
            rustls::CipherSuite::TLS13_AES_128_GCM_SHA256
                | rustls::CipherSuite::TLS13_AES_256_GCM_SHA384
        )
    });
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = ClientConfig::builder_with_provider(Arc::new(p))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.enable_secret_extraction = true;

    let mut sock = TcpStream::connect((host.as_str(), 443)).expect("connect");
    let mut conn =
        ClientConnection::new(Arc::new(config), host.clone().try_into().unwrap()).expect("client");
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).expect("handshake");
    }
    let suite = conn.negotiated_cipher_suite().unwrap().suite();
    println!(
        "handshake complete: {:?} {:?}",
        conn.protocol_version().unwrap(),
        suite
    );

    // From here on rustls is out of the picture.
    let secrets = conn.dangerous_extract_secrets().expect("extract secrets");
    let ((tx_seq, tx_secret), (rx_seq, rx_secret)) = (secrets.tx, secrets.rx);
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    let (s, k, iv) = keys(&tx_secret);
    tx.set_keys(s, &k, &iv, tx_seq).unwrap();
    let (s, k, iv) = keys(&rx_secret);
    rx.set_keys(s, &k, &iv, rx_seq).unwrap();
    println!("keys extracted: tx sequence {tx_seq}, rx sequence {rx_seq}");

    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let mut record = vec![0u8; request.len() + OVERHEAD];
    let n = tx
        .seal(CONTENT_APPLICATION_DATA, request.as_bytes(), &mut record)
        .unwrap();
    sock.write_all(&record[..n]).expect("send");

    let (mut inbox, mut plain, mut out) = (Vec::new(), Vec::new(), vec![0u8; 17000]);
    let mut counts = [0usize; 3];
    let mut chunk = [0u8; 16384];
    'read: loop {
        while let Some(len) = record_len(&inbox).expect("record header") {
            if inbox.len() < len {
                break;
            }
            let (ctype, n) = rx
                .open(&inbox[..len], &mut out[..len - HEADER_LEN - TAG_LEN])
                .expect("record must authenticate");
            inbox.drain(..len);
            match ctype {
                CONTENT_APPLICATION_DATA => {
                    counts[0] += 1;
                    plain.extend_from_slice(&out[..n]);
                }
                // Session tickets arrive after the handshake; nothing to do with them.
                CONTENT_HANDSHAKE => counts[1] += 1,
                CONTENT_ALERT => {
                    counts[2] += 1;
                    break 'read;
                }
                other => panic!("unexpected content type {other}"),
            }
        }
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => inbox.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&plain);
    let status = text.lines().next().unwrap_or("(no response)");
    println!(
        "records opened: {} application data, {} handshake, {} alert",
        counts[0], counts[1], counts[2]
    );
    println!("response: {status} ({} bytes of plaintext)", plain.len());
    assert!(
        status.starts_with("HTTP/1.1 "),
        "no HTTP response came through our record layer"
    );
    println!("ok: handshake by rustls, every record after it by osc-tls");
}
