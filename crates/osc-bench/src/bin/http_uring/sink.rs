// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The multi-threaded peer for throughput runs.

use super::*;

/// Thread-per-connection HTTP peer that never becomes the bottleneck:
/// request bodies are discarded in the kernel with MSG_TRUNC.
pub(super) fn sink(port: u16, get_size: u32, tls_cert: Option<&str>) {
    let l =
        TcpListener::bind(("0.0.0.0", port)).unwrap_or_else(|e| die(&format!("bind {port}: {e}")));
    // TLS mode: a throwaway certificate for `bench.test`, saved for the client to trust.
    let tls = tls_cert.map(|path| {
        let k = rcgen::generate_simple_self_signed(vec!["bench.test".to_string()])
            .unwrap_or_else(|e| die(&format!("certificate: {e}")));
        std::fs::write(path, k.cert.der()).unwrap_or_else(|e| die(&format!("{path}: {e}")));
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(k.signing_key.serialize_der()),
        );
        let cfg = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![k.cert.der().clone()], key)
        .unwrap_or_else(|e| die(&format!("server config: {e}")));
        std::sync::Arc::new(cfg)
    });
    eprintln!(
        "http_uring: sink listening on {port}{}",
        if tls.is_some() { " (TLS)" } else { "" }
    );
    for s in l.incoming() {
        let Ok(s) = s else { continue };
        let tls = tls.clone();
        std::thread::spawn(move || match tls {
            None => serve(s, get_size, true),
            Some(cfg) => {
                let Ok(conn) = rustls::ServerConnection::new(cfg) else {
                    return;
                };
                serve(rustls::StreamOwned::new(conn, s), get_size, false)
            }
        });
    }
}

trait MaybeFd {
    fn fd(&self) -> Option<c_int>;
}
impl MaybeFd for std::net::TcpStream {
    fn fd(&self) -> Option<c_int> {
        Some(self.as_raw_fd())
    }
}
impl MaybeFd for rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream> {
    fn fd(&self) -> Option<c_int> {
        None
    }
}

/// One connection of the sink. With `discard_in_kernel`, request bodies
/// are dropped with MSG_TRUNC and never copied; over TLS they must be
/// read and decrypted.
fn serve<S: Read + Write + MaybeFd>(mut s: S, get_size: u32, discard_in_kernel: bool) {
    {
        {
            let mut head = vec![0u8; 16 * 1024];
            let mut scratch = vec![0x5au8; 1 << 20];
            let mut have = 0usize;
            loop {
                // Read one request head.
                let end = loop {
                    if let Some(p) = head[..have].windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                    match s.read(&mut head[have..]) {
                        Ok(n) if n > 0 => have += n,
                        _ => return,
                    }
                };
                let text = String::from_utf8_lossy(&head[..end]).to_ascii_lowercase();
                let length: usize = text
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                let is_get = text.starts_with("get ");
                // Discard the body: what arrived with the head, then the rest unseen.
                let mut left = length.saturating_sub(have - end);
                have = 0;
                while left > 0 {
                    let n = match s.fd().filter(|_| discard_in_kernel) {
                        Some(fd) => unsafe {
                            recv(
                                fd,
                                scratch.as_mut_ptr().cast(),
                                left.min(1 << 30),
                                MSG_TRUNC,
                            )
                        },
                        None => s
                            .read(&mut scratch[..left.min(1 << 20)])
                            .map_or(-1, |n| n as isize),
                    };
                    if n <= 0 {
                        return;
                    }
                    left -= n as usize;
                }
                let body = if is_get { get_size as usize } else { 0 };
                if s.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {body}\r\n\r\n").as_bytes(),
                )
                .is_err()
                {
                    return;
                }
                let mut sent = 0;
                while sent < body {
                    let n = (body - sent).min(scratch.len());
                    if s.write_all(&scratch[..n]).is_err() {
                        return;
                    }
                    sent += n;
                }
            }
        }
    }
}
