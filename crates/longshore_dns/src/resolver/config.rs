// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The resolver's configuration, and the check [`Resolver::new`](super::Resolver::new) makes of it.

use super::{Family, SERVERS_MAX};
use crate::codec::{encode_query_for, MESSAGE_MAX};
use longshore_common::io::{BufId, Sock};
use std::fmt;
use std::net::SocketAddr;

/// The hosts to resolve and the resources the resolver may use.
pub struct Config {
    /// One or two nameservers. On a timeout the next one is tried.
    pub servers: Vec<SocketAddr>,
    pub hosts: Vec<String>,
    pub family: Family,
    /// Prefer addresses whose connections have large TCP segments. `None`
    /// turns this off and every address is handed out in rotation.
    /// `Some(n)` hands out the addresses whose measured segment size is at
    /// least half the largest seen and a quarter of the path's limit, and
    /// keeps at least `n` addresses in use by admitting the next largest. Sizes come from
    /// [`Resolver::connected`](super::Resolver::connected). An address not yet measured is handed out
    /// like the large ones, so that it gets measured.
    ///
    /// This puts a host's connections on fewer of its addresses.
    pub prefer_large_segments: Option<usize>,
    /// Every host is re-resolved this often, whatever its TTL says.
    pub refresh_ns: u64,
    /// How long to wait for one answer.
    pub timeout_ns: u64,
    /// One socket slot and one receive buffer per server.
    pub socks: [Sock; SERVERS_MAX],
    /// Each must hold [`MESSAGE_MAX`](crate::codec::MESSAGE_MAX) bytes: that much is asked of every
    /// receive. Checked when the resolver starts.
    pub rx_bufs: [BufId; SERVERS_MAX],
    /// Must hold the longest query: 18 bytes more than the longest
    /// hostname, so 271 at most. Checked when the resolver starts.
    pub tx_buf: BufId,
    /// Tokens used are `token_base..token_base + 256`.
    pub token_base: u64,
}

/// Why [`Resolver::new`](super::Resolver::new) refused a [`Config`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Not one or two nameservers; the number given.
    Servers(usize),
    /// No hostname to resolve.
    NoHosts,
    /// The refresh interval or the answer timeout is zero.
    ZeroInterval,
    /// A hostname that cannot be put in a DNS query.
    Hostname(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Servers(n) => write!(f, "one or two nameservers, not {n}"),
            ConfigError::NoHosts => f.write_str("at least one hostname"),
            ConfigError::ZeroInterval => {
                f.write_str("the refresh interval and the answer timeout must be above zero")
            }
            ConfigError::Hostname(h) => write!(f, "invalid hostname {h:?}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Every hostname must fit in a query for the preferred record type,
    /// which is the longest of the two.
    pub(super) fn check(&self) -> Result<(), ConfigError> {
        if !(1..=SERVERS_MAX).contains(&self.servers.len()) {
            return Err(ConfigError::Servers(self.servers.len()));
        }
        if self.hosts.is_empty() {
            return Err(ConfigError::NoHosts);
        }
        if self.timeout_ns == 0 || self.refresh_ns == 0 {
            return Err(ConfigError::ZeroInterval);
        }
        let mut probe = [0u8; MESSAGE_MAX];
        let preferred = self.family.records()[0];
        match self
            .hosts
            .iter()
            .find(|h| encode_query_for(h, 0, preferred, &mut probe).is_err())
        {
            Some(h) => Err(ConfigError::Hostname(h.clone())),
            None => Ok(()),
        }
    }
}
