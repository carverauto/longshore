// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a run of `tcp_uring rr`, `bulk` or `path` is asked to be, and
//! what follows from it: the frames of the run, their framing at each
//! end, and the pools of an end.

use super::{Kind, BLOCK, HEADER};
use crate::die::{die, OrDie};
use longshore::tcp::{ConnOpts, Framing, HeaderFraming, Rx, Sizes};
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
            segment: 65_536,
            spin: false,
            header: 0,
            body_at_block: false,
            listen: None,
            connect: None,
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
                "--rx" if pick(&flag, &value, "ring", "direct") => o.rx = Rx::Direct,
                "--rx" => o.rx = Rx::Ring,
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
        let room = self.framing(true).lead() + frame;
        let each = if self.header == 0 { 2 } else { 3 };
        let many = (2 * self.lines()).clamp(16, 8192);
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
        ConnOpts::new(Framing::Header(self.framing(false)))
            .with_rx(self.rx)
            .with_frame_on_packet(self.frame_on_packet)
    }

    /// The options of the peer's ends. A log node receives each block in
    /// frame memory of its own, to write it from there.
    pub(super) fn peer_opts(&self) -> ConnOpts {
        if self.kind == Kind::Path {
            ConnOpts::new(Framing::Header(self.framing(true))).with_rx(Rx::Direct)
        } else {
            self.conn_opts()
        }
    }
}

/// Whether `value` is `yes` rather than `no`; anything else is refused.
fn pick(flag: &str, value: &str, no: &str, yes: &str) -> bool {
    if value != no && value != yes {
        die(format_args!("{flag} takes {no} or {yes}"));
    }
    value == yes
}
