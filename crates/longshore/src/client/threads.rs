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
use crate::store::s3::{KeyPrefix, Validate};
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
    /// Host header and DNS name of the store: `bucket.endpoint`, or the
    /// endpoint's own name with [`Builder::bucket_in_path`].
    pub host: String,
    /// `Some(bucket)` addresses the bucket in the path, `/bucket/key` on
    /// `host`, as self-hosted S3-compatible stores commonly expect.
    pub bucket_in_path: Option<String>,
    pub port: u16,
    pub region: String,
    /// The credentials to start with.
    pub credentials: Credentials,
    /// A file to reread credentials from, in the form
    /// [`Credentials::parse`] reads. Each I/O thread rereads it every
    /// [`Builder::credentials_reread_secs`] and, when its contents differ
    /// from the credentials in use, tries the new ones with one request
    /// and adopts them if the store accepts them (see
    /// [`Builder::credentials_validate`]). Temporary credentials expire, so
    /// a client that runs longer than they last needs this. The path must
    /// be absolute and UTF-8.
    pub credentials_file: Option<std::path::PathBuf>,
    pub credentials_reread_secs: u64,
    /// The request that proves new credentials before they are adopted.
    /// The default lists one key in the whole bucket. Credentials that may
    /// list only under a prefix need that prefix here, or they are never
    /// adopted. [`Client::credential_reloads`] counts both outcomes.
    pub credentials_validate: Validate,
    /// Send a CRC-64/NVME with every stored body, which the store checks.
    /// On by default. It is one more pass over the body: a plain upload
    /// from one thread ran at 16 GB/s with it and 37 GB/s without. Off
    /// affects plain stores only. [`Client::store_no_clobber`] always sends
    /// one, since a retry refused because an object exists compares it to
    /// tell this call's own write from someone else's.
    pub checksums: bool,
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
            credentials_file: None,
            credentials_reread_secs: 60,
            credentials_validate: Validate::List(KeyPrefix::EMPTY),
            checksums: true,
            bucket_in_path: None,
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
        if self
            .credentials_file
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || p.to_str().is_none())
        {
            return bad("credentials_file must be an absolute path in UTF-8");
        }
        if let Some(bucket) = &self.bucket_in_path {
            let valid = (3..=63).contains(&bucket.len())
                && bucket.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'
                });
            if !valid {
                return bad("bucket_in_path is not a bucket name");
            }
        }
        if self.region.len() > 64 {
            return bad("region must be at most 64 bytes");
        }
        if self.nameservers.len() > crate::dns::resolver::SERVERS_MAX {
            return bad("at most two nameservers");
        }
        let mut query = [0u8; 512];
        if self.address.is_none()
            && crate::dns::codec::encode_query(&self.host, 0, &mut query).is_err()
        {
            return bad("host is not a name DNS can look up");
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
        if failed.is_none() && lanes.len() != self.threads as usize {
            failed = Some(std::io::Error::other(
                "an I/O thread stopped while starting",
            ));
        }
        let threads = Threads::new(lanes.clone(), joins);
        match failed {
            Some(e) => Err(BuildError::Io(e)),
            None => Ok(Client {
                lanes: lanes.into(),
                alive: Some(Arc::new(threads)),
                checksum: if self.checksums {
                    crate::store::s3::Checksum::Compute
                } else {
                    crate::store::s3::Checksum::None
                },
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
mod linux;
