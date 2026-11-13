// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The self-test: client and in-tree server on one ring.

use super::*;

/// Client and server on one ring. Large buffers: server head 0, server
/// body 1, user tx 2, user rx 3. Small buffers: the client head.
pub(super) fn selftest() {
    const SIZE: u32 = 8 << 20;
    let limits = Limits {
        sockets_max: 3,
        buffers: 4,
        buffer_size: SIZE,
        small_buffers: 1,
        small_buffer_size: 4096,
        ops_max: 8,
    };
    let mut io = UringIo::new(limits).unwrap_or_else(|e| die(&e.to_string()));
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: 1,
        first_sock: 2,
        first_buf: 0,
        head_max: 4096,
        buffer_size: SIZE,
        objects_max: 1,
        token_base: 2 << 40,
        checksum: None,
    });
    let addr = server
        .start(&mut io, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .unwrap_or_else(|e| die(&e.to_string()));
    let mut client = Client::new(client::Config {
        first_buf: limits.small(0).0,
        ..client_cfg(1, SIZE, 1)
    });
    let (tx, rx) = (BufId(2), BufId(3));
    for (i, b) in io.buf(tx).iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[2];
    }
    let mut out = Completions::new(16);
    let enters0 = io.enters();

    // Everything from here to the end of the exchange runs with allocation forbidden.
    let results = sealed(|| {
        let mut results = [Err(Error::Busy); 3];
        for (step, (method, body, resp)) in [
            ("PUT", Some((tx, SIZE)), None),
            ("GET", None, Some(rx)),
            ("HEAD", None, None),
        ]
        .into_iter()
        .enumerate()
        {
            let req = Request {
                method,
                target: "/bucket/object",
                host: "localhost",
                host_id: 0,
                addr,
                headers: &[],
                body,
                response_buf: resp,
            };
            if let Err(e) = client.start(&mut io, step as u64, &req) {
                results[step] = Err(e);
                continue;
            }
            'wait: loop {
                io.tick(true, &mut out).expect("tick");
                while let Some(c) = out.pop() {
                    match client.on_completion(&mut io, c) {
                        Handled::Done(d) => {
                            results[step] = d.result;
                            break 'wait;
                        }
                        Handled::Pending => {}
                        Handled::NotMine => {
                            server.on_completion(&mut io, c);
                        }
                    }
                }
            }
        }
        results
    });

    let statuses: Vec<String> = results
        .iter()
        .map(|r| match r {
            Ok(resp) => format!("{} ({} bytes)", resp.status, resp.body_len),
            Err(e) => format!("{e:?}"),
        })
        .collect();
    println!(
        "PUT -> {}   GET -> {}   HEAD -> {}",
        statuses[0], statuses[1], statuses[2]
    );
    let same = io.buf(tx).to_vec() == io.buf(rx).to_vec();
    let ok = matches!(results[0], Ok(r) if r.status == 200)
        && matches!(results[1], Ok(r) if r.status == 200 && r.body_len == SIZE)
        && matches!(results[2], Ok(r) if r.status == 200)
        && same
        && client.connects == 1;
    println!(
        "bytes_match={same} connections={} enters={} allocation_sealed=true",
        client.connects,
        io.enters() - enters0
    );
    println!("{}", if ok { "selftest ok" } else { "selftest FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
