// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Where a connection goes or a listener listens: an IP address and port,
//! or the path of a Unix-domain socket.

use std::net::SocketAddr;

/// The longest path a Unix-domain socket can have, its ending NUL included.
const SUN_PATH: usize = 108;

/// Why a path cannot name a Unix-domain socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathError {
    /// Empty.
    Empty,
    /// 108 bytes or more.
    TooLong,
    /// A NUL byte inside it.
    Nul,
}

/// The path of a Unix-domain socket, kept inline so that naming one
/// allocates nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UnixPath {
    /// The path followed by a NUL.
    bytes: [u8; SUN_PATH],
    len: u8,
}

impl UnixPath {
    /// The socket at `path`.
    pub fn new(path: &str) -> Result<UnixPath, PathError> {
        let p = path.as_bytes();
        if p.is_empty() {
            return Err(PathError::Empty);
        }
        if p.len() >= SUN_PATH {
            return Err(PathError::TooLong);
        }
        if p.contains(&0) {
            return Err(PathError::Nul);
        }
        let mut bytes = [0; SUN_PATH];
        bytes[..p.len()].copy_from_slice(p);
        Ok(UnixPath {
            bytes,
            len: p.len() as u8,
        })
    }

    /// The path, without its ending NUL.
    pub fn as_str(&self) -> &str {
        // Built from a `&str` and cut at its end.
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    /// The path with its ending NUL, as a connect takes it.
    pub fn with_nul(&self) -> &[u8] {
        &self.bytes[..=usize::from(self.len)]
    }
}

/// An address for `Net::connect` and `Net::listen`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Addr {
    /// TCP, at an IP address and port.
    Ip(SocketAddr),
    /// A Unix-domain stream socket, at a path.
    Unix(UnixPath),
}

impl Addr {
    /// The Unix-domain socket at `path`.
    pub fn unix(path: &str) -> Result<Addr, PathError> {
        UnixPath::new(path).map(Addr::Unix)
    }

    /// The IP address and port, for a TCP address.
    pub const fn ip(&self) -> Option<SocketAddr> {
        match self {
            Addr::Ip(a) => Some(*a),
            Addr::Unix(_) => None,
        }
    }
}

impl From<SocketAddr> for Addr {
    fn from(a: SocketAddr) -> Addr {
        Addr::Ip(a)
    }
}

impl From<UnixPath> for Addr {
    fn from(p: UnixPath) -> Addr {
        Addr::Unix(p)
    }
}

/// The name a TLS peer's certificate must match, kept inline so that
/// naming one allocates nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ServerName {
    bytes: [u8; ServerName::MAX],
    len: u8,
}

impl ServerName {
    /// The longest name: that of a DNS name.
    pub const MAX: usize = 253;

    /// No name.
    pub const NONE: ServerName = ServerName {
        bytes: [0; ServerName::MAX],
        len: 0,
    };

    /// `name`, or `None` when it is empty or longer than [`ServerName::MAX`].
    pub fn new(name: &str) -> Option<ServerName> {
        let n = name.as_bytes();
        if n.is_empty() || n.len() > ServerName::MAX {
            return None;
        }
        let mut bytes = [0; ServerName::MAX];
        bytes[..n.len()].copy_from_slice(n);
        Some(ServerName {
            bytes,
            len: n.len() as u8,
        })
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        // Built from a `&str` and cut at its end.
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}
