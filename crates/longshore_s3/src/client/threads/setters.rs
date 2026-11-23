// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The [`Builder`]'s setters. Each records one setting and returns the
//! builder, so they chain; [`Builder::build`] checks the values.

use super::Builder;
use crate::creds::Credentials;
use crate::s3::Validate;
use longshore_dns::Family;
use std::net::SocketAddr;
use std::path::PathBuf;

impl Builder {
    /// Host header and DNS name of the store: `bucket.endpoint`, or the
    /// endpoint's own name with [`Builder::bucket_in_path`].
    pub fn host(mut self, host: &str) -> Builder {
        self.host = host.into();
        self
    }

    /// `Some(bucket)` addresses the bucket in the path, `/bucket/key` on
    /// [`Builder::host`], as self-hosted S3-compatible stores commonly expect.
    pub fn bucket_in_path(mut self, bucket: Option<String>) -> Builder {
        self.bucket_in_path = bucket;
        self
    }

    /// The store's TCP port. 80 by default; HTTPS stores commonly use 443.
    pub fn port(mut self, port: u16) -> Builder {
        self.port = port;
        self
    }

    /// The region requests are signed for, such as `us-east-1`.
    pub fn region(mut self, region: &str) -> Builder {
        self.region = region.into();
        self
    }

    /// The credentials to start with.
    pub fn credentials(mut self, credentials: Credentials) -> Builder {
        self.credentials = credentials;
        self
    }

    /// A file to reread credentials from, in the form
    /// [`Credentials::parse`] reads. Each I/O thread rereads it every
    /// [`Builder::credentials_reread_secs`] and, when its contents differ
    /// from the credentials in use, tries the new ones with one request
    /// and adopts them if the store accepts them (see
    /// [`Builder::credentials_validate`]). Temporary credentials expire, so
    /// a client that runs longer than they last needs this. The path must
    /// be absolute and UTF-8.
    pub fn credentials_file(mut self, file: Option<PathBuf>) -> Builder {
        self.credentials_file = file;
        self
    }

    /// Seconds between rereads of [`Builder::credentials_file`]. 60 by default.
    pub fn credentials_reread_secs(mut self, secs: u64) -> Builder {
        self.credentials_reread_secs = secs;
        self
    }

    /// The request that proves new credentials before they are adopted.
    /// The default lists one key in the whole bucket. Credentials that may
    /// list only under a prefix need that prefix here, or they are never
    /// adopted. [`Client::credential_reloads`](crate::client::Client::credential_reloads) counts both outcomes.
    pub fn credentials_validate(mut self, validate: Validate) -> Builder {
        self.credentials_validate = validate;
        self
    }

    /// Send a CRC-64/NVME with every stored body, which the store checks.
    /// On by default. It is one more pass over the body: a plain upload
    /// from one thread ran at 16 GB/s with it and 37 GB/s without. Off
    /// affects plain stores and multipart uploads (see
    /// [`Client::upload_begin`](crate::client::Client::upload_begin)). [`Client::store_no_clobber`](crate::client::Client::store_no_clobber) always sends
    /// one, since a retry refused because an object exists compares it to
    /// tell this call's own write from someone else's.
    pub fn checksums(mut self, checksums: bool) -> Builder {
        self.checksums = checksums;
        self
    }

    /// HTTPS, verified against the public root certificates.
    pub fn tls(mut self, tls: bool) -> Builder {
        self.tls = tls;
        self
    }

    /// I/O threads. Each is an independent store with its own connections
    /// and buffers. One fills a fast card with plain uploads; more are for
    /// HTTPS and downloads, which one core bounds.
    pub fn threads(mut self, threads: u32) -> Builder {
        self.threads = threads;
        self
    }

    /// Pin thread `n` to CPU `first + n * step`. On a machine with several
    /// NUMA nodes, choose CPUs on the network card's node. Refused on macOS.
    pub fn pin(mut self, first_step: Option<(usize, usize)>) -> Builder {
        self.pin = first_step;
        self
    }

    /// Use the portable readiness backend (`epoll` on Linux) instead of
    /// `io_uring`. On macOS the portable backend (`kqueue`) is always used.
    pub fn portable(mut self, portable: bool) -> Builder {
        self.portable = portable;
        self
    }

    /// Whether [`Builder::portable`] is set.
    pub fn is_portable(&self) -> bool {
        self.portable
    }

    /// Connections per I/O thread. Like the limits after it, this one is
    /// per thread.
    pub fn connections(mut self, connections: u32) -> Builder {
        self.connections = connections;
        self
    }

    /// Buffers lent to callers. An operation holds one while it runs.
    pub fn buffers(mut self, buffers: u32) -> Builder {
        self.buffers = buffers;
        self
    }

    /// Bytes per buffer, which is the largest object one operation moves.
    pub fn buffer_size(mut self, bytes: u32) -> Builder {
        self.buffer_size = bytes;
        self
    }

    /// Operations in progress at most. Others wait.
    pub fn operations(mut self, operations: u32) -> Builder {
        self.operations = operations;
        self
    }

    /// Waiters preallocated per pool on each thread. Waiting beyond this
    /// grows the queue rather than refusing callers.
    pub fn waiters(mut self, waiters: u32) -> Builder {
        self.waiters = waiters;
        self
    }

    /// The address family to resolve and connect with. IPv4 by default.
    pub fn family(mut self, family: Family) -> Builder {
        self.family = family;
        self
    }

    /// See [`longshore_dns::Config::prefer_large_segments`].
    pub fn prefer_large_segments(mut self, bytes: Option<usize>) -> Builder {
        self.prefer_large_segments = bytes;
        self
    }

    /// Nameservers to ask. Empty reads `/etc/resolv.conf`.
    pub fn nameservers(mut self, nameservers: Vec<SocketAddr>) -> Builder {
        self.nameservers = nameservers;
        self
    }

    /// Connect here and resolve nothing, for a store at a known address.
    pub fn address(mut self, address: Option<SocketAddr>) -> Builder {
        self.address = address;
        self
    }
}
