// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One I/O thread of a client: its backend, its store, and its loop.

use super::{Builder, Lane};
use crate::client::worker::{Layout, Worker};
use crate::s3::{Config, CredentialsFile, Endpoint, Vendor, S3};
use longshore_common::io::posix::PosixIo;
#[cfg(target_os = "linux")]
use longshore_common::io::uring::UringIo;
use longshore_common::io::{BufId, Completions, Io, Limits, Sock};
use longshore_dns::Resolver;
use longshore_http_client::client as http;
#[cfg(feature = "tls")]
use longshore_tls::handshake::ClientSetup;
use std::sync::mpsc::Sender;
use std::sync::Arc;

const MS: u64 = 1_000_000;

#[cfg(target_os = "linux")]
extern "C" {
    fn sched_setaffinity(pid: i32, len: usize, mask: *const u64) -> i32;
}

/// Platform I/O backend that a client thread builds and drives.
pub(super) trait Backend: Io + Sized {
    fn build(limits: Limits) -> std::io::Result<Self>;
    fn quiesce(&mut self) -> bool;
    #[cfg(all(feature = "fault-injection", target_os = "linux"))]
    fn inject_lane_fault(&mut self, _code: u8) {}
}

#[cfg(target_os = "linux")]
impl Backend for UringIo {
    fn build(limits: Limits) -> std::io::Result<Self> {
        UringIo::new(limits)
    }

    fn quiesce(&mut self) -> bool {
        UringIo::quiesce(self)
    }

    #[cfg(feature = "fault-injection")]
    fn inject_lane_fault(&mut self, code: u8) {
        use longshore_common::io::uring::Fault;
        match code {
            1 => self.inject(Fault::TickFails),
            2 => self.inject(Fault::RingLost),
            _ => {}
        }
    }
}

impl Backend for PosixIo {
    fn build(limits: Limits) -> std::io::Result<Self> {
        PosixIo::new(limits)
    }

    fn quiesce(&mut self) -> bool {
        // The POSIX world only touches caller buffers during a synchronous
        // syscall inside `tick`, never asynchronously in the kernel.
        true
    }
}

/// One I/O thread: build everything, report the lane, then run until
/// shut down.
pub fn run<B: Backend>(b: &Builder, n: u32, ready: Sender<std::io::Result<Arc<Lane>>>) {
    pin(b, n);
    let (mut io, mut worker, lane) = match setup::<B>(b) {
        Ok(built) => built,
        Err(e) => {
            // The builder may have stopped waiting; this thread ends either way.
            let _: Option<()> = ready.send(Err(e)).ok();
            return;
        }
    };
    #[cfg(all(feature = "fault-injection", target_os = "linux"))]
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
        #[cfg(all(feature = "fault-injection", target_os = "linux"))]
        {
            let code = asked.fault.swap(0, std::sync::atomic::Ordering::AcqRel);
            if code != 0 {
                io.inject_lane_fault(code);
            }
        }
        if io.tick(true, &mut out).is_err() {
            // A broken backend. Stop what the kernel still runs, then hand
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
            let _: bool = worker.on_completion(&mut io, c);
        }
    }
    worker.close();
}

/// Pin thread `n` to its core, if the builder asks for pinning.
fn pin(b: &Builder, n: u32) {
    #[cfg(target_os = "linux")]
    if let Some((first, step)) = b.pin {
        let cpu = first + n as usize * step;
        let mut mask = [0u64; 16];
        if cpu < 1024 {
            mask[cpu / 64] |= 1 << (cpu % 64);
            let _: i32 = unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _: (&Builder, u32) = (b, n);
}

/// Build the backend, the store and the worker, and start the worker.
fn setup<B: Backend>(b: &Builder) -> std::io::Result<(B, Worker, Arc<Lane>)> {
    let (limits, tls_buffers) = limits(b);
    let mut io = B::build(limits)?;
    io.measure_segment_sizes(b.prefer_large_segments.is_some());
    let credentials_file = credentials_file(b, &mut io, limits)?;
    let endpoint = endpoint(b, limits)?;
    let mut config = store_config(b, limits, endpoint);
    config.credentials_file = credentials_file;
    let s3 = S3::new(config);
    let layout = Layout {
        operations: b.operations,
        first_buffer: BufId(tls_buffers),
        buffers: b.buffers,
        token_base: 7 << 40,
        heartbeat_ns: 50 * MS,
    };
    let (mut worker, lane) = Worker::with_waiters(&mut io, s3, layout, b.waiters as usize)
        .map_err(std::io::Error::other)?;
    worker.start(&mut io);
    Ok((io, worker, lane))
}

/// The backend's limits, and how many large buffers TLS takes before the
/// lent ones.
fn limits(b: &Builder) -> (Limits, u32) {
    let conns = b.connections;
    // Large buffers: two per connection for TLS records, then the lent
    // ones. Small buffers: one head per connection, three for DNS, one for
    // rereading the credentials file.
    let tls_buffers = if cfg!(feature = "tls") && b.tls {
        2 * conns
    } else {
        0
    };
    let buffer_size = b.buffer_size.max(if cfg!(feature = "tls") && b.tls {
        256 * 1024
    } else {
        8192
    });
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
        ..Limits::BASE
    };
    (limits, tls_buffers)
}

/// The credentials file to reread, registered with the backend, if any.
fn credentials_file<B: Backend>(
    b: &Builder,
    io: &mut B,
    limits: Limits,
) -> std::io::Result<Option<CredentialsFile>> {
    let Some(path) = &b.credentials_file else {
        return Ok(None);
    };
    let file = io.register_file(&path.to_string_lossy())?;
    Ok(Some(CredentialsFile {
        file,
        buf: limits.small(b.connections + 3),
        reread_ns: b.credentials_reread_secs.max(1) * 1000 * MS,
        token: 6 << 40,
        validate: b.credentials_validate,
    }))
}

/// The fixed address, or a resolver for the host name.
fn endpoint(b: &Builder, limits: Limits) -> std::io::Result<Endpoint> {
    let conns = b.connections;
    if let Some(addr) = b.address {
        return Ok(Endpoint::Fixed(addr));
    }
    let resolver = Resolver::new(longshore_dns::Config {
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
    })
    // The host name or the nameservers cannot be used: the builder is
    // told why, as for any other start-up failure.
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    Ok(Endpoint::Resolved(resolver))
}

/// The store's configuration for this thread, without the credentials file.
fn store_config(b: &Builder, limits: Limits, endpoint: Endpoint) -> Config {
    let buffer_size = limits.buffer_size;
    Config {
        vendor: Vendor::S3,
        host: b.host.clone(),
        bucket_in_path: b.bucket_in_path.clone(),
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
            connections_max: b.connections,
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
        #[cfg(feature = "tls")]
        tls: b.tls.then(|| (ClientSetup::with_public_roots(), 0)),
        endpoint,
    }
}
