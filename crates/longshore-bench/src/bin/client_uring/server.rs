// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The in-tree server on its own ring and thread, serving until the process
//! ends.

use crate::die::OrDie;
use crate::{exact, Args, CALLERS, CONNS_PER_THREAD, SIZE};
use longshore::http::server::{self, Server};
use longshore::io::uring::UringIo;
use longshore::io::{Completions, Io, Limits, Sock};
use std::net::SocketAddr;
use std::sync::mpsc::channel;

/// Start the server and return the address it listens on.
pub fn start(args: Args) -> SocketAddr {
    let Args {
        path_style, rotate, ..
    } = args;
    let loopback = args.loopback();
    let (bound, listening) = channel();
    let server_conns = args.threads * CONNS_PER_THREAD;
    let _: std::thread::JoinHandle<()> = std::thread::spawn(move || {
        let mut io = UringIo::new(Limits {
            sockets_max: 1 + server_conns,
            buffers: 2 * server_conns,
            buffer_size: SIZE,
            ops_max: server_conns * 3 + 2,
            ..Limits::BASE
        })
        .or_die("server ring");
        let mut server = Server::new(server::Config {
            listener: Sock(0),
            connections_max: server_conns,
            first_sock: 1,
            first_buf: 0,
            head_max: 4096,
            buffer_size: SIZE,
            objects_max: (CALLERS * 2) as u32,
            token_base: 2 << 40,
            // The server keeps and checks the checksum the client sends.
            checksum: Some(|body: &[u8]| {
                longshore::checksum::crc64_header_value(longshore::checksum::crc64_nvme(body))
            }),
        });
        // And the SHA-256 of a store that sends one, and it names versions.
        server.check_sha256(exact::sha256_header);
        server.keep_versions();
        if path_style {
            server.serve_bucket_in_path("media");
        }
        if rotate {
            server.allow_access_keys(&["AKIDSECOND"]);
        }
        let addr = server
            .start(&mut io, SocketAddr::new(loopback, 0))
            .or_die("server start");
        bound.send(addr).or_die("send");
        let mut out = Completions::new(64);
        loop {
            io.tick(true, &mut out).or_die("server tick");
            while let Some(c) = out.pop() {
                let _: bool = server.on_completion(&mut io, c);
            }
        }
    });
    listening.recv().or_die("the server did not start")
}
