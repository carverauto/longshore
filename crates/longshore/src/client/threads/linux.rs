// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One I/O thread of a client on Linux: its ring, its store and its loop.

use super::{Builder, Lane};
use crate::client::worker::{Layout, Worker};
use crate::dns::Resolver;
use crate::http::client as http;
use crate::io::uring::UringIo;
use crate::io::{BufId, Completions, Io, Limits, Sock};
use crate::store::s3::{Config, CredentialsFile, Endpoint, Vendor, S3};
use crate::tls::handshake::ClientSetup;
use std::sync::mpsc::Sender;
use std::sync::Arc;

const MS: u64 = 1_000_000;

extern "C" {
    fn sched_setaffinity(pid: i32, len: usize, mask: *const u64) -> i32;
}

/// One I/O thread: build everything, report the lane, then run until
/// shut down.
pub fn run(b: Builder, n: u32, ready: Sender<std::io::Result<Arc<Lane>>>) {
    if let Some((first, step)) = b.pin {
        let cpu = first + n as usize * step;
        let mut mask = [0u64; 16];
        if cpu < 1024 {
            mask[cpu / 64] |= 1 << (cpu % 64);
            unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
        }
    }
    let conns = b.connections;
    // Large buffers: two per connection for TLS records, then the lent
    // ones. Small buffers: one head per connection, three for DNS, one for
    // rereading the credentials file.
    let tls_buffers = if b.tls { 2 * conns } else { 0 };
    let buffer_size = b.buffer_size.max(if b.tls { 256 * 1024 } else { 8192 });
    let limits = Limits {
        sockets_max: conns + 2,
        buffers: tls_buffers + b.buffers,
        buffer_size,
        small_buffers: conns + 4,
        small_buffer_size: 8192,
        // Per connection: a send, a receive and a close or cancel. Then
        // the resolver's four, the store's timer, the worker's two and
        // the read of the credentials file.
        ops_max: conns * 3 + 4 + 1 + 2 + 1,
    };
    let mut io = match UringIo::new(limits) {
        Ok(io) => io,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    io.measure_segment_sizes(b.prefer_large_segments.is_some());
    let credentials_file = match &b.credentials_file {
        None => None,
        Some(path) => match io.register_file(&path.to_string_lossy()) {
            Ok(file) => Some(CredentialsFile {
                file,
                buf: limits.small(conns + 3),
                reread_ns: b.credentials_reread_secs.max(1) * 1000 * MS,
                token: 6 << 40,
                validate: b.credentials_validate,
            }),
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        },
    };
    let endpoint = match b.address {
        Some(addr) => Endpoint::Fixed(addr),
        None => Endpoint::Resolved(Resolver::new(crate::dns::Config {
            servers: b.nameservers.clone(),
            hosts: vec![b.host.clone()],
            family: b.family,
            prefer_large_segments: b.prefer_large_segments,
            refresh_ns: 30_000 * MS,
            timeout_ns: 2_000 * MS,
            socks: [Sock(conns), Sock(conns + 1)],
            rx_bufs: [limits.small(conns), limits.small(conns + 1)],
            tx_buf: limits.small(conns + 2),
            token_base: 3 << 40,
        })),
    };
    let s3 = S3::new(Config {
        vendor: Vendor::S3,
        host: b.host.clone(),
        bucket_in_path: b.bucket_in_path.clone(),
        port: b.port,
        region: b.region.clone(),
        credentials: b.credentials,
        credentials_file,
        requests_max: b.operations,
        attempts_max: 4,
        backoff_ns: 200 * MS,
        backoff_max_ns: 5_000 * MS,
        require_checksums: false,
        http: http::Config {
            connections_max: conns,
            first_sock: 0,
            first_buf: limits.small(0).0,
            head_max: 8192,
            buffer_size,
            max_age_ns: 300_000 * MS,
            max_requests: 100_000,
            request_timeout_ns: 60_000 * MS,
            zero_copy_min: 64 * 1024,
            token_base: 1 << 40,
            tls_batch: buffer_size.min(3 << 20),
            tls_records_max: http::TLS_RECORDS_MAX,
            check_idle: true,
        },
        tls: b.tls.then(|| (ClientSetup::with_public_roots(), 0)),
        endpoint,
    });
    let layout = Layout {
        operations: b.operations,
        first_buffer: BufId(tls_buffers),
        buffers: b.buffers,
        token_base: 7 << 40,
        heartbeat_ns: 50 * MS,
    };
    let (mut worker, lane) = Worker::new(&mut io, s3, layout);
    worker.start(&mut io);
    #[cfg(feature = "fault-injection")]
    let asked = lane.clone();
    if ready.send(Ok(lane)).is_err() {
        return;
    }
    drop(ready);
    let mut out = Completions::new(64);
    while !worker.finished() {
        worker.turn(&mut io);
        if worker.finished() {
            break;
        }
        #[cfg(feature = "fault-injection")]
        match asked.fault.swap(0, std::sync::atomic::Ordering::AcqRel) {
            1 => io.inject(crate::io::uring::Fault::TickFails),
            2 => io.inject(crate::io::uring::Fault::RingLost),
            _ => {}
        }
        if io.tick(true, &mut out).is_err() {
            // A broken ring. Stop what the kernel still runs, then hand
            // callers their operations back. The pool stays allocated
            // until they shut the thread down, or for good if the kernel
            // could not be stopped.
            worker.close();
            let quiet = io.quiesce();
            worker.abandon(quiet);
            worker.refuse_until_shutdown();
            return;
        }
        while let Some(c) = out.pop() {
            worker.on_completion(&mut io, c);
        }
    }
    worker.close();
}
