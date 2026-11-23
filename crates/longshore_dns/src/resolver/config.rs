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

mod fields;

/// The hosts to resolve and the resources the resolver may use. Made with
/// [`Config::new`] and the `with_*` setters; [`Resolver::new`](super::Resolver::new)
/// checks it.
pub struct Config {
    pub(crate) servers: Vec<SocketAddr>,
    pub(crate) hosts: Vec<String>,
    pub(crate) family: Family,
    pub(crate) prefer_large_segments: Option<usize>,
    pub(crate) refresh_ns: u64,
    pub(crate) timeout_ns: u64,
    pub(crate) socks: [Sock; SERVERS_MAX],
    pub(crate) rx_bufs: [BufId; SERVERS_MAX],
    pub(crate) tx_buf: BufId,
    pub(crate) token_base: u64,
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
