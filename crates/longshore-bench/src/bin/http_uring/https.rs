// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One HTTPS GET through the full stack to a real server.

use super::*;
use crate::die::OrDie;

/// Run the loop until the request is done; returns its result and how
/// many completions (other than the heartbeat's) it took. With `trace`,
/// each completion is printed.
fn exchange(
    io: &mut UringIo,
    client: &mut Client,
    out: &mut Completions,
    trace: bool,
) -> (Result<client::Response, Error>, u32) {
    let mut completions = 0u32;
    let result = sealed(|| loop {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == HEARTBEAT {
                client.tick(io);
                io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
                    .or_die("submit");
                continue;
            }
            completions += 1;
            if trace {
                // Tracing allocates; it is a debugging aid only.
                longshore::io::alloc_guard::allow_alloc(|| {
                    eprintln!(
                        "completion token={:#x} result={}",
                        c.token().get(),
                        c.result()
                    )
                });
            }
            if let Handled::Done(d) = client.on_completion(io, c) {
                return d.result();
            }
        }
    });
    (result, completions)
}

/// Buffers: client head 0, TLS records 1 and 2, response 3.
pub(super) fn https(host: &str, ip: &str, path: &str) {
    const SIZE: u32 = 1 << 20;
    let addr = SocketAddr::new(
        ip.parse()
            .unwrap_or_else(|_| die("IPV4 must be an address")),
        443,
    );
    let mut io = UringIo::new(
        Limits::BASE
            .with_sockets_max(1)
            .with_buffers(4)
            .with_buffer_size(SIZE)
            .with_ops_max(8),
    )
    .unwrap_or_else(|e| die(e));
    let cfg = client_cfg(1, SIZE, 0, 0)
        .request_timeout_ns(10_000 * MS)
        .build()
        .or_die("the client configuration");
    let mut client = Client::with_tls(
        cfg,
        longshore::tls::handshake::ClientSetup::with_public_roots(),
        1,
    );
    let mut out = Completions::new(16);
    let req = Request::new("GET", path, host, 0, addr).with_response_buf(Some(BufId::new(3)));
    client
        .start(&mut io, 1, &req)
        .unwrap_or_else(|e| die(format_args!("start: {e:?}")));
    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
        .or_die("submit");
    let t0 = io.now_ns();
    // Read before sealing: looking up an environment variable allocates.
    let trace = std::env::var_os("LONGSHORE_TRACE").is_some();
    let (result, completions) = exchange(&mut io, &mut client, &mut out, trace);
    let ms = (io.now_ns() - t0) / MS;
    match result {
        Ok(r) => {
            let n = (r.body_len() as usize).min(120);
            let body =
                String::from_utf8_lossy(&io.buf(BufId::new(3))[..n]).replace(['\n', '\r'], " ");
            println!("https://{host}{path} -> {} ({} bytes) in {ms} ms, {completions} completions: {body}", r.status(), r.body_len());
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
