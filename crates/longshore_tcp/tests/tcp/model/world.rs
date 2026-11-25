// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The two sets of a run and their pools, with TLS or without.

use super::drive::FRAME_MAX;
use longshore_common::io::Limits;
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::check::World;
use longshore_tcp::Sizes;

/// The name the server's certificate is for.
pub(crate) const NAME: &str = "replica.longshore.test";

/// Small pools: three connections a side, three frame buffers, three
/// segments, two ring buffers, two queued sends a connection. With TLS a
/// frame buffer holds a record and a segment two, the least there is.
fn sizes(connects: u32, accepts: u32, listeners: u32, tls: bool) -> Sizes {
    let (frame, segment) = if tls {
        (32 << 10, 64 << 10)
    } else {
        (8192, FRAME_MAX as u32)
    };
    Sizes::new(connects, accepts, listeners)
        .with_frames(3, frame)
        .with_segments(3, segment)
        .with_ring(2, 1024)
        .with_header_max(256)
        .with_spans(2)
        .with_tls(tls)
}

/// Whether the run of `seed` protects its connections: one in three,
/// where TLS is compiled in.
pub(crate) const fn protected(seed: u64) -> bool {
    cfg!(feature = "tls-core") && seed % 3 == 1
}

/// The server's key: an Ed25519 key in PKCS #8, the same in every run.
/// A run can only be replayed when every message of the handshake has
/// the length it had before, which an ECDSA signature does not. (A key
/// for tests alone: it is written out here.)
#[cfg(feature = "tls-core")]
const KEY: [u8; 48] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
    0x6c, 0x6f, 0x6e, 0x67, 0x73, 0x68, 0x6f, 0x72, 0x65, 0x20, 0x74, 0x63, 0x70, 0x20, 0x6d, 0x6f,
    0x64, 0x65, 0x6c, 0x20, 0x74, 0x65, 0x73, 0x74, 0x20, 0x6b, 0x65, 0x79, 0x20, 0x30, 0x30, 0x31,
];

#[cfg(feature = "tls-core")]
fn certs() -> &'static longshore_tcp::check::Certs {
    use longshore_tcp::check::must;
    use longshore_tls::handshake::{ClientSetup, ServerSetup};
    static CERTS: std::sync::OnceLock<longshore_tcp::check::Certs> = std::sync::OnceLock::new();
    CERTS.get_or_init(|| {
        let pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
            &KEY.as_slice().into(),
            &rcgen::PKCS_ED25519,
        );
        let pair = must(pair, "key");
        let mut params = must(
            rcgen::CertificateParams::new(vec![NAME.to_string()]),
            "certificate",
        );
        params.serial_number = Some(rcgen::SerialNumber::from(1u64));
        let cert = must(params.self_signed(&pair), "certificate")
            .der()
            .to_vec();
        let client = must(ClientSetup::with_roots(&[&cert]), "client setup");
        let server = must(ServerSetup::new(&[&cert], &KEY), "server setup");
        longshore_tcp::check::Certs::new(client, server, NAME)
    })
}

/// The world of the run of `seed`.
pub(crate) fn world(seed: u64, faults: Faults) -> World<SimIo> {
    let tls = protected(seed);
    let (client, server) = (sizes(3, 0, 0, tls), sizes(0, 3, 2, tls));
    let mut make = |limits| SimIo::new(limits, seed, faults);
    #[cfg(feature = "tls-core")]
    if tls {
        return World::new_tls(&mut make, Limits::BASE, client, server, certs());
    }
    World::new(&mut make, Limits::BASE, client, server)
}
