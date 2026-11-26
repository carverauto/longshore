// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The two sets of a run and their pools, with TLS or without.

use super::drive::FRAME_MAX;
use longshore_common::io::Limits;
use longshore_common::sim::{Faults, SimIo};
use longshore_tcp::check::World;
use longshore_tcp::{ConnOpts, Framing, HeaderFraming, Reconnect, Rx, Sizes};

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

/// The faults a run is under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Campaign {
    /// Reports out of order, receives and sends cut short, and resets in
    /// one run in four.
    Usual,
    /// Resets, often enough to fall inside frames.
    Resets,
    /// Most receives and sends cut short.
    Slow,
    /// Operations that are ready held back, in any order.
    Delays,
    /// Connects refused.
    Refusals,
    /// Bits flipped in transit, over TLS, which must notice every one.
    /// The one campaign whose runs do not repeat by seed: see
    /// `the_same_seed_gives_the_same_trace`. A failing seed of it may
    /// take several replays to fail again.
    Flips,
}

impl Campaign {
    /// Every campaign that can run: `Flips` only where TLS is compiled in.
    pub fn all() -> &'static [Campaign] {
        const ALL: [Campaign; 6] = [
            Campaign::Usual,
            Campaign::Resets,
            Campaign::Slow,
            Campaign::Delays,
            Campaign::Refusals,
            Campaign::Flips,
        ];
        &ALL[..ALL.len() - usize::from(!cfg!(feature = "tls-core"))]
    }

    fn faults(self, seed: u64) -> Faults {
        let usual = Faults::NONE
            .with_reorder(300_000)
            .with_partial_recv(200_000)
            .with_short_send(200_000);
        match self {
            Campaign::Usual if seed.is_multiple_of(4) => usual.with_reset(2_000),
            Campaign::Usual => usual,
            Campaign::Resets => usual.with_reset(20_000),
            Campaign::Slow => usual.with_partial_recv(700_000).with_short_send(700_000),
            Campaign::Delays => usual.with_delay(400_000),
            Campaign::Refusals => usual.with_connect_refused(300_000),
            Campaign::Flips => usual.with_bit_flip(3_000),
        }
    }

    /// Whether a stream can break at any byte, so that an end in order
    /// says nothing of how much arrived before it.
    pub(crate) fn breaks(self, seed: u64) -> bool {
        match self {
            Campaign::Usual => seed.is_multiple_of(4),
            Campaign::Resets | Campaign::Flips => true,
            Campaign::Slow | Campaign::Delays | Campaign::Refusals => false,
        }
    }

    /// Whether the run of `seed` protects its connections: one in three
    /// where TLS is compiled in, and every run with flipped bits.
    pub(crate) fn protected(self, seed: u64) -> bool {
        cfg!(feature = "tls-core") && (self == Campaign::Flips || seed % 3 == 1)
    }
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

/// The world of the run of `seed` in `campaign`.
pub(crate) fn world(campaign: Campaign, seed: u64) -> World<SimIo> {
    let (tls, faults) = (campaign.protected(seed), campaign.faults(seed));
    let (client, server) = (sizes(3, 0, 0, tls), sizes(0, 3, 2, tls));
    let mut make = |limits| SimIo::new(limits, seed, faults);
    #[cfg(feature = "tls-core")]
    if tls {
        return World::new_tls(&mut make, Limits::BASE, client, server, certs());
    }
    World::new(&mut make, Limits::BASE, client, server)
}

/// With TLS a frame can also be longer than a record, so that it comes
/// in two and is put together from both.
pub(crate) const ACROSS_RECORDS: usize = 20_000;
const FRAME_MAX_TLS: usize = 24_000;

/// The options `bits` choose, and whether they have half-close.
pub(crate) fn opts(bits: u32, tls: bool) -> Result<(ConnOpts, bool), String> {
    opts_with(bits, false, tls)
}

/// [`opts`], for a connection that dials again when it goes down if
/// `reconnect` is set.
pub(crate) fn opts_with(bits: u32, reconnect: bool, tls: bool) -> Result<(ConnOpts, bool), String> {
    let longest = if tls { FRAME_MAX_TLS } else { FRAME_MAX };
    let mut framing = HeaderFraming::length_prefix(longest as u32).map_err(|e| format!("{e:?}"))?;
    let rx = if bits & 1 == 0 { Rx::Ring } else { Rx::Direct };
    if rx == Rx::Direct && bits & 0x20 != 0 {
        // Half the direct connections lay a frame so that its body starts
        // at a block: nothing else about them may differ.
        framing = framing
            .with_body_at_block(64)
            .map_err(|e| format!("{e:?}"))?;
    }
    let half_close = bits & 2 != 0;
    // One connection in four ends when it has been idle for a while.
    let idle = if bits & 24 == 24 { 20_000_000 } else { 0 };
    let opts = ConnOpts::new(Framing::Header(framing))
        .with_rx(rx)
        .with_half_close(half_close)
        .with_idle_timeout_ns(idle)
        .with_reconnect(reconnect.then(|| Reconnect::new(1_000_000, 8_000_000, 100)))
        .with_connect_timeout_ns(50_000_000);
    Ok((opts, half_close))
}
