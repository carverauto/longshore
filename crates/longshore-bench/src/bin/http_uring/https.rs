// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One HTTPS GET through the full stack to a real server.

use super::*;

/// Buffers: client head 0, TLS records 1 and 2, response 3.
pub(super) fn https(host: &str, ip: &str, path: &str) {
    const SIZE: u32 = 1 << 20;
    let addr = SocketAddr::new(
        ip.parse()
            .unwrap_or_else(|_| die("IPV4 must be an address")),
        443,
    );
    let mut io = UringIo::new(Limits {
        sockets_max: 1,
        buffers: 4,
        buffer_size: SIZE,
        small_buffers: 0,
        small_buffer_size: 0,
        ops_max: 8,
    })
    .unwrap_or_else(|e| die(&e.to_string()));
    let mut cfg = client_cfg(1, SIZE, 0);
    cfg.request_timeout_ns = 10_000 * MS;
    let mut client = Client::with_tls(
        cfg,
        longshore::tls::handshake::ClientSetup::with_public_roots(),
        1,
    );
    let mut out = Completions::new(16);
    let req = Request {
        method: "GET",
        target: path,
        host,
        host_id: 0,
        addr,
        headers: &[],
        body: None,
        response_buf: Some(BufId(3)),
    };
    client
        .start(&mut io, 1, &req)
        .unwrap_or_else(|e| die(&format!("start: {e:?}")));
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
        .unwrap();
    let t0 = io.now_ns();
    let mut completions = 0u32;
    // Read before sealing: looking up an environment variable allocates.
    let trace = std::env::var_os("LONGSHORE_TRACE").is_some();
    let result = sealed(|| loop {
        io.tick(true, &mut out).expect("tick");
        while let Some(c) = out.pop() {
            if c.token.0 == HEARTBEAT {
                client.tick(&mut io);
                io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
                    .unwrap();
                continue;
            }
            completions += 1;
            if trace {
                // Tracing allocates; it is a debugging aid only.
                longshore::io::alloc_guard::allow_alloc(|| {
                    eprintln!("completion token={:#x} result={}", c.token.0, c.result)
                });
            }
            if let Handled::Done(d) = client.on_completion(&mut io, c) {
                return d.result;
            }
        }
    });
    let ms = (io.now_ns() - t0) / MS;
    match result {
        Ok(r) => {
            let n = (r.body_len as usize).min(120);
            let body = String::from_utf8_lossy(&io.buf(BufId(3))[..n]).replace(['\n', '\r'], " ");
            println!("https://{host}{path} -> {} ({} bytes) in {ms} ms, {completions} completions: {body}", r.status, r.body_len);
        }
        Err(e) => {
            println!(
                "https://{host}{path} -> FAILED {e:?} after {ms} ms, {completions} completions"
            );
            std::process::exit(1);
        }
    }
    std::process::exit(0);
}
