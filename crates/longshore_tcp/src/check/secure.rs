// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Worlds whose connections are protected with TLS.

use super::{fail, World};
use crate::{Addr, KernelTransport, ServerName, Sizes, Tls};
use longshore_common::io::{Io, Limits};
use longshore_tls::handshake::{ClientSetup, ServerSetup};

/// What the two sets of a world need for TLS.
#[derive(Clone)]
pub struct Certs {
    client: ClientSetup,
    server: ServerSetup,
    name: ServerName,
}

impl Certs {
    /// The client verifies with `client` that the certificate `server`
    /// presents is `name`'s.
    pub fn new(client: ClientSetup, server: ServerSetup, name: &str) -> Certs {
        match ServerName::new(name) {
            Some(name) => Certs {
                client,
                server,
                name,
            },
            None => fail(format_args!("not a server name: {name:?}")),
        }
    }

    /// Both sides' setups, for a transport that has both ends.
    pub(crate) fn both(&self) -> Tls {
        Tls::new()
            .with_client(self.client.clone())
            .with_server(self.server.clone())
    }

    /// The name the client expects.
    pub(crate) const fn name(&self) -> ServerName {
        self.name
    }
}

/// Sizes with room for TLS: segments that hold two records, frame memory
/// that holds one, and ring buffers far smaller than a record.
pub fn sizes(connects: u32, accepts: u32, listeners: u32) -> Sizes {
    Sizes::new(connects, accepts, listeners)
        .with_frames(8, 32 << 10)
        .with_segments(8, 64 << 10)
        .with_ring(4, 1024)
        .with_header_max(256)
        .with_spans(4)
        .with_tls(true)
}

impl<IO: Io> World<IO> {
    /// A world whose connections are protected with `certs`.
    pub fn new_tls(
        make: &mut impl FnMut(Limits) -> IO,
        base: Limits,
        client: Sizes,
        server: Sizes,
        certs: &Certs,
    ) -> World<IO> {
        let wrap = |t: KernelTransport<IO>, is_client: bool| {
            t.with_tls(if is_client {
                Tls::new().with_client(certs.client.clone())
            } else {
                Tls::new().with_server(certs.server.clone())
            })
        };
        World::build(make, base, (client, server), Some(certs.name), wrap)
    }

    /// The usual world with TLS, about to listen on `addr`.
    pub fn listening_on_tls(
        make: &mut impl FnMut(Limits) -> IO,
        addr: &Addr,
        certs: &Certs,
    ) -> World<IO> {
        if let Addr::Unix(path) = addr {
            // Usually there is none.
            drop(std::fs::remove_file(path.as_str()));
        }
        World::new_tls(make, Limits::BASE, sizes(4, 0, 0), sizes(2, 4, 2), certs)
    }
}
