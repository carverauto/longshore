// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a run of `tcp_uring rr`, `bulk` or `path` is asked to be, and
//! what follows from it: the frames of the run, their framing at each
//! end, and the pools of an end.

use super::{Kind, BLOCK, HEADER};
use crate::die::{die, OrDie};
use longshore::tcp::{ConnOpts, Framing, HeaderFraming, Rx, Sizes, TLS_SEGMENT_MIN};
use longshore::tls::provider::Provider;
use std::net::SocketAddr;

/// Where the length of a frame lies in a header of `path`, as in
/// UltraGraphDB's: four bytes, little-endian, counting the whole frame.
const LEN_AT: u32 = 40;

/// What a run is asked to be.
#[derive(Clone)]
pub(crate) struct Opts {
    pub(crate) kind: Kind,
    pub(crate) dsync: bool,
    pub(crate) dir: String,
    pub(crate) epoll: bool,
    pub(crate) secs: u64,
    pub(crate) conns: u32,
    pub(crate) body: u32,
    pub(crate) frame: u32,
    pub(crate) rx: Rx,
    pub(crate) ring: u32,
    pub(crate) zc_min: u32,
    /// Zero-copy sends a connection may have with their memory still out.
    pub(crate) zc_sends: u32,
    /// Each frame a send of its own, with `TCP_NODELAY` on both ends.
    pub(crate) frame_on_packet: bool,
    /// With `--rx zc`: the interface's index and the receive queue the
    /// peer's area is registered for.
    pub(crate) zc_queue: Option<(u32, u32)>,
    pub(crate) segment: u32,
    pub(crate) spin: bool,
    /// path: bytes of the header in front of a block, or zero for a
    /// block that is the whole frame behind a four-byte prefix.
    pub(crate) header: u32,
    /// path: the node receives a frame so that its body lies at a block
    /// boundary, rather than moving the body to one.
    pub(crate) body_at_block: bool,
    /// The peer alone, at this address.
    pub(super) listen: Option<SocketAddr>,
    /// The load alone, against a peer at this address.
    pub(crate) connect: Option<SocketAddr>,
    /// Over TLS, with the certificate in this file (`tls.rs`).
    pub(crate) tls: Option<String>,
    /// What seals and opens the records of a run over TLS.
    pub(crate) provider: Provider,
}

impl Opts {
    fn new(kind: Kind) -> Opts {
        Opts {
            kind,
            dsync: false,
            dir: std::env::temp_dir().to_string_lossy().into_owned(),
            epoll: false,
            secs: 3,
            conns: 1,
            body: 64,
            frame: 65_536,
            rx: Rx::Ring,
            ring: 4096,
            zc_min: 16_384,
            zc_sends: 4,
            frame_on_packet: false,
            zc_queue: None,
            segment: 65_536,
            spin: false,
            header: 0,
            body_at_block: false,
            listen: None,
            connect: None,
            tls: None,
            provider: Provider::preferred(),
        }
    }

    /// The length of every frame of the run, its header included.
    pub(crate) const fn frame_len(&self) -> u32 {
        match self.kind {
            Kind::Rr => 4 + HEADER + self.body,
            Kind::Bulk => self.frame,
            Kind::Path => self.header + BLOCK,
        }
    }

    /// The length of the frame in which a replica says what it is: a
    /// header alone, or eight bytes behind a prefix.
    pub(crate) const fn hello_len(&self) -> u32 {
        if self.header == 0 {
            8
        } else {
            self.header
        }
    }

    /// Write the length of a frame of `len` bytes where the run's
    /// framing reads it.
    pub(crate) fn write_len(&self, to: &mut [u8], len: u32) {
        if self.header == 0 {
            to[..4].copy_from_slice(&(len - 4).to_be_bytes());
        } else {
            let at = LEN_AT as usize;
            to[at..at + 4].copy_from_slice(&len.to_le_bytes());
        }
    }

    pub(super) fn parse(cmd: &str, mut args: impl Iterator<Item = String>) -> Opts {
        let mut o = Opts::new(match cmd {
            "bulk" => Kind::Bulk,
            "path" => Kind::Path,
            _ => Kind::Rr,
        });
        let mut body_at = false;
        while let Some(flag) = args.next() {
            let value = args.next().or_die(&format!("{flag} needs a value"));
            let number = || {
                value
                    .parse::<u32>()
                    .ok()
                    .or_die(&format!("{flag} takes a number"))
            };
            let addr = || {
                value
                    .parse::<SocketAddr>()
                    .ok()
                    .or_die(&format!("{flag} takes an address and a port"))
            };
            match flag.as_str() {
                "--backend" => o.epoll = pick(&flag, &value, "uring", "epoll"),
                "--secs" => o.secs = u64::from(number()),
                "--conns" => o.conns = number().max(1),
                "--body" => o.body = number(),
                "--frame" => o.frame = number().max(8),
                "--rx" => {
                    o.rx = match value.as_str() {
                        "ring" => Rx::Ring,
                        "direct" => Rx::Direct,
                        "zc" => Rx::ZeroCopy,
                        _ => die("--rx takes ring, direct or zc"),
                    }
                }
                "--zc-queue" => o.zc_queue = Some(zc_queue(&value)),
                "--ring" => o.ring = number().max(64),
                "--zc-min" => o.zc_min = number(),
                "--zc-sends" => o.zc_sends = number(),
                "--frame-on-packet" => o.frame_on_packet = pick(&flag, &value, "off", "on"),
                "--segment" => o.segment = number(),
                "--wait" => o.spin = pick(&flag, &value, "sleep", "spin"),
                "--sync" => o.dsync = pick(&flag, &value, "fsync", "dsync"),
                "--dir" => o.dir.clone_from(&value),
                "--header" => o.header = number(),
                "--body-at" => {
                    o.body_at_block = pick(&flag, &value, "frame", "block");
                    body_at = true;
                }
                "--listen" => o.listen = Some(addr()),
                "--connect" => o.connect = Some(addr()),
                "--tls" => o.tls = Some(value.clone()),
                "--provider" => o.provider = provider(&value),
                other => die(format_args!("unknown flag {other}")),
            }
        }
        o.check(body_at);
        o
    }

    /// Refuse what cannot be run. `body_at` is whether `--body-at` was
    /// given.
    fn check(&self, body_at: bool) {
        if self.listen.is_some() && self.connect.is_some() {
            die("--listen and --connect are two processes, not one");
        }
        if self.kind == Kind::Path && self.conns > 500 {
            // A replica is sent every source's block; its queue of sends
            // holds two for each, up to the thousand a connection can.
            die("path takes up to 500 connections");
        }
        if self.rx == Rx::ZeroCopy
            && (self.listen.is_none() || self.zc_queue.is_none() || self.kind == Kind::Path)
        {
            // A queue of a card is registered, which loopback is not; a log
            // node forwards what it receives, which a frame in pages cannot
            // be (zc_uring log writes from pages).
            die("--rx zc is of a peer alone (--listen) of rr or bulk, with --zc-queue IFACE:QUEUE");
        }
        if self.tls.is_some() {
            // Records are not frames: a frame in pages has nothing to lend
            // (`Net::connect_tls`). A log node's replicas are not wired for it.
            if self.rx == Rx::ZeroCopy || self.kind == Kind::Path {
                die("--tls is of rr and bulk, without --rx zc");
            }
            if self.segment.max(self.frame_len()) < TLS_SEGMENT_MIN {
                die(format_args!(
                    "--tls needs --segment {TLS_SEGMENT_MIN} at least"
                ));
            }
        }
        if self.header != 0 && self.kind != Kind::Path {
            die("--header is of path");
        }
        if body_at && self.header == 0 {
            die("--body-at is of a path with a header: --header 256");
        }
        // The header holds its length field, and fits the header area a
        // connection set has unless it is asked for a larger one.
        if self.header != 0 && !(LEN_AT + 4..=256).contains(&self.header) {
            die("--header takes 0, or 44 to 256");
        }
    }

    /// Connections of a run: with `path`, three replicas beside the
    /// sources.
    pub(crate) fn lines(&self) -> u32 {
        self.conns + if self.kind == Kind::Path { 3 } else { 0 }
    }

    /// Sends a connection can have queued. A replica of a log node is
    /// sent every source's block, all of them at once when their writes
    /// complete together: its queue holds two for each source.
    fn spans(&self) -> u32 {
        if self.kind == Kind::Path {
            (2 * self.conns + 8).clamp(16, 1024)
        } else {
            16
        }
    }

    /// The pools of one end: `connects` it opens, `accepts` it takes.
    /// Frame memory holds what lies in front of a frame whose body is at
    /// a block boundary, and the frame. With a header, a log node may
    /// hold a third piece for each source: the one it moves a body to.
    pub(super) fn sizes(&self, connects: u32, accepts: u32) -> Sizes {
        let frame = self.frame_len();
        let mut room = self.framing(true).lead() + frame;
        if self.tls.is_some() {
            // A record is opened whole into frame memory.
            room = room.max(longshore::stream::RING_DST_MIN);
        }
        let each = if self.header == 0 { 2 } else { 3 };
        let many = (2 * self.lines()).clamp(16, 8192);
        let zc = self.zc_room(accepts);
        Sizes::new(connects, accepts, u32::from(accepts > 0))
            .with_frames(
                (each * self.lines()).clamp(16, 8192),
                room.next_multiple_of(4096),
            )
            .with_segments(many, self.segment.max(frame))
            .with_ring((4 * self.lines()).clamp(64, 16_384), self.ring)
            .with_spans(self.spans())
            .with_zero_copy_min(self.zc_min)
            .with_zero_copy_sends(self.zc_sends, if self.zc_sends == 0 { 0 } else { 256 })
            .with_zc(zc.0, zc.1, zc.2)
            .with_tls(self.tls.is_some())
    }

    /// Pages, frames and piece records for the peer of `--rx zc`: two
    /// frames a connection in pages at once, each of whole pages and a
    /// record a page and two more; none for any other run, or the load.
    fn zc_room(&self, accepts: u32) -> (u32, u32, u32) {
        if self.rx != Rx::ZeroCopy || accepts == 0 {
            return (0, 0, 0);
        }
        let frames = 2 * self.lines();
        let pages = self.frame_len().div_ceil(4096);
        (frames * pages, frames, (frames * (pages + 2)).min(32_000))
    }

    /// How the run's frames are cut from the stream: behind a four-byte
    /// prefix, or, for a path with a header, by the length inside it. At
    /// the peer, if asked, with the body at a block boundary.
    pub(crate) fn framing(&self, at_peer: bool) -> HeaderFraming {
        let max = self.frame_len();
        let framing = if self.header == 0 {
            HeaderFraming::length_prefix(max)
        } else {
            HeaderFraming::new(self.header, LEN_AT, 4, false, true, max)
        };
        let laid = if at_peer && self.body_at_block {
            framing.and_then(|f| f.with_body_at_block(BLOCK))
        } else {
            framing
        };
        laid.ok().or_die("framing")
    }

    pub(crate) fn conn_opts(&self) -> ConnOpts {
        // The load receives direct when its peer receives in pages.
        let rx = if self.rx == Rx::ZeroCopy {
            Rx::Direct
        } else {
            self.rx
        };
        ConnOpts::new(Framing::Header(self.framing(false)))
            .with_rx(rx)
            .with_frame_on_packet(self.frame_on_packet)
    }

    /// The options of the peer's ends. A log node receives each block in
    /// frame memory of its own, to write it from there.
    pub(super) fn peer_opts(&self) -> ConnOpts {
        if self.kind == Kind::Path {
            ConnOpts::new(Framing::Header(self.framing(true))).with_rx(Rx::Direct)
        } else {
            self.conn_opts().with_rx(self.rx)
        }
    }
}

/// `IFACE:QUEUE`, the interface by its name, as its index.
fn zc_queue(value: &str) -> (u32, u32) {
    extern "C" {
        fn if_nametoindex(name: *const core::ffi::c_char) -> u32;
    }
    let (name, queue) = value.split_once(':').or_die("--zc-queue takes IFACE:QUEUE");
    let queue = queue.parse().ok().or_die("--zc-queue: the queue, a number");
    let c = std::ffi::CString::new(name)
        .ok()
        .or_die("--zc-queue: the interface");
    let index = unsafe { if_nametoindex(c.as_ptr()) };
    if index == 0 {
        die(format_args!("--zc-queue: no interface {name}"));
    }
    (index, queue)
}

/// The provider `--provider` names, if this build has it.
fn provider(value: &str) -> Provider {
    let named = Provider::ALL.into_iter().find(|p| p.feature() == value);
    match named {
        Some(p) if p.compiled() => p,
        Some(_) => die(format_args!("--provider {value} is not in this build")),
        None => die("--provider takes aws-lc, ring or rustls"),
    }
}

/// Whether `value` is `yes` rather than `no`; anything else is refused.
fn pick(flag: &str, value: &str, no: &str, yes: &str) -> bool {
    if value != no && value != yes {
        die(format_args!("{flag} takes {no} or {yes}"));
    }
    value == yes
}
