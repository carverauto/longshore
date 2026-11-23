// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The in-tree object server on a thread of its own, over the portable
//! backend on loopback, and a client builder pointed at it: the rig for
//! tests of the threaded client on real sockets.

use longshore_common::io::posix::PosixIo;
use longshore_common::io::{Completions, Io, Limits, Op, Token, Wake};
use longshore_http_client::server::{self, Server};
use longshore_s3::client::Builder;
use longshore_s3::creds::Credentials;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread::JoinHandle;

const WAKE_TOKEN: Token = Token(9 << 40);

/// A server running on its own thread until [`ServerThread::stop`].
pub struct ServerThread {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    wake: Arc<dyn Wake>,
    thread: JoinHandle<()>,
}

impl ServerThread {
    /// Starts a server with `config` on a `PosixIo` with `limits`, listening
    /// on a free loopback port, and returns once it listens.
    pub fn start(limits: Limits, config: server::Config) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = stop.clone();
        let (ready_tx, ready_rx) = channel();
        let thread = std::thread::spawn(move || {
            let mut io = PosixIo::new(limits).expect("server PosixIo");
            let waker = io.waker().expect("server waker");
            let mut server = Server::new(config);
            let addr = server
                .start(&mut io, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                .expect("server start");
            io.submit(WAKE_TOKEN, Op::AwaitWake).expect("arm wake");
            ready_tx.send((addr, waker)).unwrap();
            serve_until_stopped(&mut io, &mut server, &stop_server);
        });
        let (addr, wake) = ready_rx.recv().expect("server did not start");
        ServerThread {
            addr,
            stop,
            wake,
            thread,
        }
    }

    /// Tells the server thread to stop, wakes it, and joins it.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Release);
        self.wake.wake();
        self.thread.join().expect("server thread");
    }

    /// A client builder for this server with `connections` connections of
    /// `buffer_size` buffers, on the portable backend on Linux too.
    pub fn builder(&self, connections: u32, buffer_size: u32) -> Builder {
        let mut b = Builder::new(
            "bucket.objects.test",
            "us-test-1",
            Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
        );
        b.port = self.addr.port();
        b.address = Some(self.addr);
        b.connections = connections;
        b.buffer_size = buffer_size;
        #[cfg(target_os = "linux")]
        {
            b.portable = true;
        }
        b
    }
}

/// Serves until `stop` is set, re-arming the wake each time it fires.
fn serve_until_stopped(io: &mut PosixIo, server: &mut Server, stop: &AtomicBool) {
    let mut out = Completions::new(32);
    while !stop.load(Ordering::Acquire) {
        io.tick(true, &mut out).expect("server tick");
        while let Some(c) = out.pop() {
            if c.token == WAKE_TOKEN {
                if !stop.load(Ordering::Acquire) {
                    io.submit(WAKE_TOKEN, Op::AwaitWake).expect("rearm wake");
                }
            } else {
                let _: bool = server.on_completion(io, c);
            }
        }
    }
}
