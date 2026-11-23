// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Can an existing TLS library do only the handshake, and our record layer
//! do everything after it?
//!
//! Handshake with rustls over a blocking socket, extract the negotiated
//! traffic keys, then speak HTTP through `longshore::tls::record` alone.
//!
//!   cargo run -p longshore-bench --example tls_probe -- HOST [PATH]

use longshore::tls::record::{
    record_len, Direction, Suite, CONTENT_ALERT, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE,
    HEADER_LEN, OVERHEAD, TAG_LEN,
};
use rustls::crypto::aws_lc_rs as provider;
use rustls::{ClientConfig, ClientConnection, ConnectionTrafficSecrets, RootCertStore};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "ConnectionTrafficSecrets is foreign and non_exhaustive: only AES-GCM suites are offered"
)]
fn keys(s: &ConnectionTrafficSecrets) -> Result<(Suite, Vec<u8>, [u8; 12])> {
    Ok(match s {
        ConnectionTrafficSecrets::Aes128Gcm { key, iv } => (
            Suite::Aes128Gcm,
            key.as_ref().to_vec(),
            iv.as_ref().try_into()?,
        ),
        ConnectionTrafficSecrets::Aes256Gcm { key, iv } => (
            Suite::Aes256Gcm,
            key.as_ref().to_vec(),
            iv.as_ref().try_into()?,
        ),
        _ => return Err("only AES-GCM suites are offered".into()),
    })
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let host = args.next().ok_or("usage: tls_probe HOST [PATH]")?;
    let path = args.next().unwrap_or_else(|| "/".into());

    let (mut sock, conn) = handshake(&host)?;
    // From here on rustls is out of the picture.
    let (mut tx, mut rx) = extract_keys(conn)?;
    println!("keys extracted");

    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let mut record = vec![0u8; request.len() + OVERHEAD];
    let n = tx
        .seal(CONTENT_APPLICATION_DATA, request.as_bytes(), &mut record)
        .map_err(|e| format!("seal the request: {e:?}"))?;
    sock.write_all(&record[..n])
        .map_err(|e| format!("send: {e}"))?;

    let (plain, counts) = read_response(&mut sock, &mut rx)?;
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
    println!("ok: handshake by rustls, every record after it by longshore::tls");
    Ok(())
}

/// A rustls client for TLS 1.3 with AES-GCM suites only, its traffic
/// secrets extractable.
fn client_config() -> Result<ClientConfig> {
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
        .map_err(|e| format!("protocol versions: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.enable_secret_extraction = true;
    Ok(config)
}

/// Connect to `host` on port 443 and complete the handshake with rustls.
fn handshake(host: &str) -> Result<(TcpStream, ClientConnection)> {
    let config = client_config()?;
    let mut sock = TcpStream::connect((host, 443)).map_err(|e| format!("connect: {e}"))?;
    let mut conn = ClientConnection::new(
        Arc::new(config),
        rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|e| format!("server name {host}: {e}"))?,
    )
    .map_err(|e| format!("client: {e}"))?;
    while conn.is_handshaking() {
        let _: (usize, usize) = conn
            .complete_io(&mut sock)
            .map_err(|e| format!("handshake: {e}"))?;
    }
    let suite = conn
        .negotiated_cipher_suite()
        .ok_or("no cipher suite negotiated")?
        .suite();
    println!(
        "handshake complete: {:?} {:?}",
        conn.protocol_version()
            .ok_or("no protocol version negotiated")?,
        suite
    );
    Ok((sock, conn))
}

/// The send and receive record directions, keyed from the connection's
/// traffic secrets.
fn extract_keys(conn: ClientConnection) -> Result<(Direction, Direction)> {
    let secrets = conn
        .dangerous_extract_secrets()
        .map_err(|e| format!("extract secrets: {e}"))?;
    let ((tx_seq, tx_secret), (rx_seq, rx_secret)) = (secrets.tx, secrets.rx);
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    let (tx_suite, tx_key, tx_iv) = keys(&tx_secret)?;
    tx.set_keys(tx_suite, &tx_key, &tx_iv, tx_seq)
        .map_err(|e| format!("send keys: {e:?}"))?;
    let (rx_suite, rx_key, rx_iv) = keys(&rx_secret)?;
    rx.set_keys(rx_suite, &rx_key, &rx_iv, rx_seq)
        .map_err(|e| format!("receive keys: {e:?}"))?;
    Ok((tx, rx))
}

/// Read and open records until an alert or the end of the stream: the
/// application data, and the count of application data, handshake and
/// alert records.
fn read_response(sock: &mut TcpStream, rx: &mut Direction) -> Result<(Vec<u8>, [usize; 3])> {
    let (mut inbox, mut plain, mut out) = (Vec::new(), Vec::new(), vec![0u8; 17000]);
    let mut counts = [0usize; 3];
    let mut chunk = [0u8; 16384];
    'read: loop {
        while let Some(len) = record_len(&inbox).map_err(|e| format!("record header: {e:?}"))? {
            if inbox.len() < len {
                break;
            }
            let (ctype, opened) = rx
                .open(&inbox[..len], &mut out[..len - HEADER_LEN - TAG_LEN])
                .map_err(|e| format!("record must authenticate: {e:?}"))?;
            let _: std::vec::Drain<'_, u8> = inbox.drain(..len);
            match ctype {
                CONTENT_APPLICATION_DATA => {
                    counts[0] += 1;
                    plain.extend_from_slice(&out[..opened]);
                }
                // Session tickets arrive after the handshake; nothing to do with them.
                CONTENT_HANDSHAKE => counts[1] += 1,
                CONTENT_ALERT => {
                    counts[2] += 1;
                    break 'read;
                }
                other => return Err(format!("unexpected content type {other}").into()),
            }
        }
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => inbox.extend_from_slice(&chunk[..read]),
        }
    }
    Ok((plain, counts))
}
