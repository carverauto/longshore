// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The I/O threads behind a [`Client`]: building them, and stopping them
//! when the last handle, buffer and operation are gone.

use super::lane::{Kind, Lane, Request, KEY_MAX};
use super::Client;
use crate::dns::Family;
use crate::store::creds::Credentials;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Why a client could not be built.
#[derive(Debug)]
pub enum BuildError {
    /// Only Linux has the I/O this client is built on.
    Unsupported,
    /// A setting is out of range. The text says which.
    Setting(&'static str),
    /// No nameserver was given and none was found in `/etc/resolv.conf`.
    NoNameserver,
    /// An I/O thread could not set up its ring.
    Io(std::io::Error),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Unsupported => write!(f, "this client runs on Linux only"),
            BuildError::Setting(what) => write!(f, "{what}"),
            BuildError::NoNameserver => write!(f, "no nameserver configured or found"),
            BuildError::Io(e) => write!(f, "I/O thread set-up failed: {e}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Settings for a [`Client`]. Fill in the fields that differ from
/// [`Builder::new`]'s, then call [`Builder::build`].
#[derive(Clone)]
pub struct Builder {
    /// Host header and DNS name of the store: `bucket.endpoint`.
    pub host: String,
    pub port: u16,
    pub region: String,
    pub credentials: Credentials,
    /// HTTPS, verified against the public root certificates.
    pub tls: bool,
    /// I/O threads. Each is an independent store with its own connections
    /// and buffers. One fills a fast card with plain uploads; more are for
    /// HTTPS and downloads, which one core bounds.
    pub threads: u32,
    /// Pin thread `n` to CPU `first + n * step`. On a machine with several
    /// NUMA nodes, choose CPUs on the network card's node.
    pub pin: Option<(usize, usize)>,
    /// The limits below are per thread.
    pub connections: u32,
    /// Buffers lent to callers. An operation holds one while it runs.
    pub buffers: u32,
    /// Bytes per buffer, which is the largest object one operation moves.
    pub buffer_size: u32,
    /// Operations in progress at most. Others wait.
    pub operations: u32,
    pub family: Family,
    /// See [`crate::dns::Config::prefer_large_segments`].
    pub prefer_large_segments: Option<usize>,
    /// Nameservers to ask. Empty reads `/etc/resolv.conf`.
    pub nameservers: Vec<SocketAddr>,
    /// Connect here and resolve nothing, for a store at a known address.
    pub address: Option<SocketAddr>,
}

impl Builder {
    /// One I/O thread, 64 connections, 64 buffers of 8 MiB, plain HTTP on
    /// port 80, IPv4.
    pub fn new(host: &str, region: &str, credentials: Credentials) -> Builder {
        Builder {
            host: host.into(),
            port: 80,
            region: region.into(),
            credentials,
            tls: false,
            threads: 1,
            pin: None,
            connections: 64,
            buffers: 64,
            buffer_size: 8 << 20,
            operations: 64,
            family: Family::V4,
            prefer_large_segments: None,
            nameservers: Vec::new(),
            address: None,
        }
    }

    fn check(&self) -> Result<(), BuildError> {
        let bad = |what| Err(BuildError::Setting(what));
        if self.threads == 0 || self.threads > 1024 {
            return bad("threads must be 1 to 1024");
        }
        if self.connections == 0 || self.buffers == 0 || self.operations == 0 {
            return bad("connections, buffers and operations must be at least 1");
        }
        if self.buffer_size < 8192 {
            return bad("buffer_size must be at least 8192");
        }
        if self.host.is_empty() || self.host.len() > 253 {
            return bad("host must be 1 to 253 bytes");
        }
        Ok(())
    }

    /// Start the I/O threads and return a handle to them. Everything the
    /// client will use is allocated here.
    pub fn build(mut self) -> Result<Client, BuildError> {
        self.check()?;
        if self.address.is_none() && self.nameservers.is_empty() {
            let conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
            self.nameservers = crate::dns::codec::parse_resolv_conf(&conf)
                .into_iter()
                .take(2)
                .map(|ip| SocketAddr::new(ip, 53))
                .collect();
            if self.nameservers.is_empty() {
                return Err(BuildError::NoNameserver);
            }
        }
        self.start()
    }

    #[cfg(not(target_os = "linux"))]
    fn start(self) -> Result<Client, BuildError> {
        Err(BuildError::Unsupported)
    }

    #[cfg(target_os = "linux")]
    fn start(self) -> Result<Client, BuildError> {
        let (ready, started) = std::sync::mpsc::channel();
        let mut joins = Vec::new();
        for n in 0..self.threads {
            let (settings, report) = (self.clone(), ready.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("longshore-io-{n}"))
                .spawn(move || linux::run(settings, n, report));
            match spawned {
                Ok(j) => joins.push(j),
                Err(e) => {
                    // The threads already started stop when their lanes do.
                    drop(ready);
                    let lanes: Vec<Arc<Lane>> = started.iter().flatten().collect();
                    drop(Threads::new(lanes, joins));
                    return Err(BuildError::Io(e));
                }
            }
        }
        drop(ready);
        let mut lanes = Vec::new();
        let mut failed = None;
        for started in started.iter().take(self.threads as usize) {
            match started {
                Ok(lane) => lanes.push(lane),
                Err(e) => failed = Some(e),
            }
        }
        let threads = Threads::new(lanes.clone(), joins);
        match failed {
            Some(e) => Err(BuildError::Io(e)),
            None => Ok(Client {
                lanes: lanes.into(),
                alive: Some(Arc::new(threads)),
            }),
        }
    }
}

/// Owns the I/O threads. Dropped when the last client handle, buffer and
/// operation are gone, since each of those keeps it alive.
pub(super) struct Threads {
    lanes: Vec<Arc<Lane>>,
    joins: Mutex<Vec<JoinHandle<()>>>,
}

impl Threads {
    #[cfg(target_os = "linux")]
    fn new(lanes: Vec<Arc<Lane>>, joins: Vec<JoinHandle<()>>) -> Threads {
        Threads {
            lanes,
            joins: Mutex::new(joins),
        }
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        for lane in &self.lanes {
            // The queue keeps one place beyond the operation slots for this.
            let _ = lane.requests.try_send(Request {
                op: 0,
                kind: Kind::Shutdown,
                key: [0; KEY_MAX],
                key_len: 0,
                buf: 0,
                len: 0,
            });
            lane.wake.wake();
        }
        for j in self.joins.lock().expect("joins lock").drain(..) {
            let _ = j.join();
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::super::worker::{Layout, Worker};
    use super::{Builder, Lane};
    use crate::dns::Resolver;
    use crate::http::client as http;
    use crate::io::uring::UringIo;
    use crate::io::{BufId, Completions, Io, Limits, Sock};
    use crate::store::s3::{Config, Endpoint, Vendor, S3};
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
        // ones. Small buffers: one head per connection, three for DNS.
        let tls_buffers = if b.tls { 2 * conns } else { 0 };
        let buffer_size = b.buffer_size.max(if b.tls { 256 * 1024 } else { 8192 });
        let limits = Limits {
            sockets_max: conns + 2,
            buffers: tls_buffers + b.buffers,
            buffer_size,
            small_buffers: conns + 3,
            small_buffer_size: 8192,
            // Per connection: a send, a receive and a close or cancel. Then
            // the resolver's four, the store's timer and the worker's two.
            ops_max: conns * 3 + 4 + 1 + 2,
        };
        let mut io = match UringIo::new(limits) {
            Ok(io) => io,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        io.measure_segment_sizes(b.prefer_large_segments.is_some());
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
            port: b.port,
            region: b.region.clone(),
            credentials: b.credentials,
            credentials_file: None,
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
            // A failed tick is a broken ring: stop, and let callers see Closed.
            if io.tick(true, &mut out).is_err() {
                break;
            }
            while let Some(c) = out.pop() {
                worker.on_completion(&mut io, c);
            }
        }
        worker.close();
    }
}
