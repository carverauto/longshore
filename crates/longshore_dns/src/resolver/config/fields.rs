// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a [`Config`] is made, changed and read.

use super::super::{Family, SERVERS_MAX};
use super::Config;
use longshore_common::io::{BufId, Sock};
use std::net::SocketAddr;

impl Config {
    /// A configuration that resolves `hosts` with `servers`, using the
    /// sockets `socks`, the receive buffers `rx_bufs`, the send buffer
    /// `tx_buf` and the tokens from `token_base`. It asks for IPv4
    /// addresses only, hands every address out in rotation, re-resolves
    /// every 30 seconds and waits 2 seconds for an answer; the `with_*`
    /// setters change that.
    pub fn new(
        servers: Vec<SocketAddr>,
        hosts: Vec<String>,
        socks: [Sock; SERVERS_MAX],
        rx_bufs: [BufId; SERVERS_MAX],
        tx_buf: BufId,
        token_base: u64,
    ) -> Config {
        Config {
            servers,
            hosts,
            family: Family::V4,
            prefer_large_segments: None,
            refresh_ns: 30_000_000_000,
            timeout_ns: 2_000_000_000,
            socks,
            rx_bufs,
            tx_buf,
            token_base,
        }
    }

    /// One or two nameservers. On a timeout the next one is tried.
    pub fn servers(&self) -> &[SocketAddr] {
        &self.servers
    }

    /// This configuration with these nameservers.
    #[must_use]
    pub fn with_servers(mut self, servers: Vec<SocketAddr>) -> Config {
        self.servers = servers;
        self
    }

    /// The hostnames to resolve.
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// This configuration resolving these hostnames.
    #[must_use]
    pub fn with_hosts(mut self, hosts: Vec<String>) -> Config {
        self.hosts = hosts;
        self
    }

    /// Which address families are asked for, and which is preferred.
    #[inline]
    pub fn family(&self) -> Family {
        self.family
    }

    /// This configuration asking for `family`.
    #[must_use]
    pub fn with_family(mut self, family: Family) -> Config {
        self.family = family;
        self
    }

    /// Prefer addresses whose connections have large TCP segments. `None`
    /// turns this off and every address is handed out in rotation.
    /// `Some(n)` hands out the addresses whose measured segment size is at
    /// least half the largest seen and a quarter of the path's limit, and
    /// keeps at least `n` addresses in use by admitting the next largest.
    /// Sizes come from [`Resolver::connected`](crate::Resolver::connected).
    /// An address not yet measured is handed out like the large ones, so
    /// that it gets measured.
    ///
    /// This puts a host's connections on fewer of its addresses.
    #[inline]
    pub fn prefer_large_segments(&self) -> Option<usize> {
        self.prefer_large_segments
    }

    /// This configuration with [`Config::prefer_large_segments`] set to `keep`.
    #[must_use]
    pub fn with_prefer_large_segments(mut self, keep: Option<usize>) -> Config {
        self.prefer_large_segments = keep;
        self
    }

    /// Every host is re-resolved this often, whatever its TTL says.
    #[inline]
    pub fn refresh_ns(&self) -> u64 {
        self.refresh_ns
    }

    /// This configuration re-resolving every `refresh_ns`.
    #[must_use]
    pub fn with_refresh_ns(mut self, refresh_ns: u64) -> Config {
        self.refresh_ns = refresh_ns;
        self
    }

    /// How long to wait for one answer.
    #[inline]
    pub fn timeout_ns(&self) -> u64 {
        self.timeout_ns
    }

    /// This configuration waiting `timeout_ns` for an answer.
    #[must_use]
    pub fn with_timeout_ns(mut self, timeout_ns: u64) -> Config {
        self.timeout_ns = timeout_ns;
        self
    }

    /// One socket slot per server.
    #[inline]
    pub fn socks(&self) -> [Sock; SERVERS_MAX] {
        self.socks
    }

    /// This configuration using the socket slots `socks`.
    #[must_use]
    pub fn with_socks(mut self, socks: [Sock; SERVERS_MAX]) -> Config {
        self.socks = socks;
        self
    }

    /// One receive buffer per server. Each must hold
    /// [`MESSAGE_MAX`](crate::codec::MESSAGE_MAX) bytes: that much is asked
    /// of every receive. Checked when the resolver starts.
    #[inline]
    pub fn rx_bufs(&self) -> [BufId; SERVERS_MAX] {
        self.rx_bufs
    }

    /// This configuration receiving into `rx_bufs`.
    #[must_use]
    pub fn with_rx_bufs(mut self, rx_bufs: [BufId; SERVERS_MAX]) -> Config {
        self.rx_bufs = rx_bufs;
        self
    }

    /// The send buffer. Must hold the longest query: 18 bytes more than the
    /// longest hostname, so 271 at most. Checked when the resolver starts.
    #[inline]
    pub fn tx_buf(&self) -> BufId {
        self.tx_buf
    }

    /// This configuration sending from `tx_buf`.
    #[must_use]
    pub fn with_tx_buf(mut self, tx_buf: BufId) -> Config {
        self.tx_buf = tx_buf;
        self
    }

    /// Tokens used are `token_base..token_base + 256`.
    #[inline]
    pub fn token_base(&self) -> u64 {
        self.token_base
    }

    /// This configuration using the tokens from `token_base`.
    #[must_use]
    pub fn with_token_base(mut self, token_base: u64) -> Config {
        self.token_base = token_base;
        self
    }
}
