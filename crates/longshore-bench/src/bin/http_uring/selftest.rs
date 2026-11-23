// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The self-test: client and in-tree server on one ring.

use super::*;
use crate::die::OrDie;

const SIZE: u32 = 8 << 20;

/// PUT `tx` to the server at `addr`, GET it back into `rx`, then HEAD it,
/// each request run to its end before the next starts.
fn exchange(
    io: &mut UringIo,
    client: &mut Client,
    server: &mut Server,
    out: &mut Completions,
    addr: SocketAddr,
    (tx, rx): (BufId, BufId),
) -> [Result<client::Response, Error>; 3] {
    let mut results = [Err(Error::Busy); 3];
    for (step, (method, body, resp)) in [
        ("PUT", Some((tx, SIZE)), None),
        ("GET", None, Some(rx)),
        ("HEAD", None, None),
    ]
    .into_iter()
    .enumerate()
    {
        let req = Request::new(method, "/bucket/object", "localhost", 0, addr)
            .with_body(body)
            .with_response_buf(resp);
        if let Err(e) = client.start(io, step as u64, &req) {
            results[step] = Err(e);
            continue;
        }
        'wait: loop {
            io.tick(true, out).or_die("tick");
            while let Some(c) = out.pop() {
                match client.on_completion(io, c) {
                    Handled::Done(d) => {
                        results[step] = d.result();
                        break 'wait;
                    }
                    Handled::Pending => {}
                    Handled::NotMine => {
                        let _: bool = server.on_completion(io, c);
                    }
                }
            }
        }
    }
    results
}

/// Client and server on one ring. Large buffers: server head 0, server
/// body 1, user tx 2, user rx 3. Small buffers: the client head.
pub(super) fn selftest() {
    let limits = Limits {
        sockets_max: 3,
        buffers: 4,
        buffer_size: SIZE,
        small_buffers: 1,
        small_buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).unwrap_or_else(|e| die(e));
    let mut server = Server::new(
        server::ServerConfig::builder(Sock(0), 1, 2, 0, SIZE, 2 << 40)
            .head_max(4096)
            .objects_max(1)
            .build()
            .or_die("server config"),
    );
    let addr = server
        .start(&mut io, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .unwrap_or_else(|e| die(e));
    let mut client = Client::new(
        client_cfg(1, SIZE, 1, limits.small(0).0)
            .build()
            .or_die("the client configuration"),
    );
    let (tx, rx) = (BufId(2), BufId(3));
    for (i, b) in io.buf(tx).iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[2];
    }
    let mut out = Completions::new(16);
    let enters0 = io.enters();

    // Everything from here to the end of the exchange runs with allocation forbidden.
    let results = sealed(|| exchange(&mut io, &mut client, &mut server, &mut out, addr, (tx, rx)));

    let statuses: Vec<String> = results
        .iter()
        .map(|r| match r {
            Ok(resp) => format!("{} ({} bytes)", resp.status(), resp.body_len()),
            Err(e) => format!("{e:?}"),
        })
        .collect();
    println!(
        "PUT -> {}   GET -> {}   HEAD -> {}",
        statuses[0], statuses[1], statuses[2]
    );
    let same = io.buf(tx).to_vec() == io.buf(rx).to_vec();
    let ok = matches!(results[0], Ok(r) if r.status() == 200)
        && matches!(results[1], Ok(r) if r.status() == 200 && r.body_len() == SIZE)
        && matches!(results[2], Ok(r) if r.status() == 200)
        && same
        && client.connects() == 1;
    println!(
        "bytes_match={same} connections={} enters={} allocation_sealed=true",
        client.connects(),
        io.enters() - enters0
    );
    println!("{}", if ok { "selftest ok" } else { "selftest FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
