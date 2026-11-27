// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How a connection behaves, and how a listener listens: the options
//! `connect` and `listen` take.

use crate::framing::Framing;

/// How a connection reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rx {
    /// As [`Rx::Direct`], but a frame whose blocks arrive lying in whole
    /// pages of the backend's receive area is lent as those pages, with
    /// no copy by the kernel or by the set; any other frame is moved once
    /// into frame memory. Needs a backend with a receive area and a set
    /// sized for it (`Sizes::with_zc`); plain TCP with header framing only.
    ZeroCopy,
    /// From the backend's shared buffer ring: a frame lying whole in one
    /// ring buffer is lent in place until the next `poll`; one that spans
    /// buffers is assembled in frame memory. For many connections with
    /// small frames.
    Ring,
    /// Each frame straight into frame memory: its header, then its body.
    /// No frame is copied; two receives per frame. For large frames.
    Direct,
}

/// When and how often a lost connection is dialled again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reconnect {
    pub(crate) base_ns: u64,
    pub(crate) max_ns: u64,
    pub(crate) jitter_per_mille: u32,
}

impl Reconnect {
    /// Wait `base_ns`, doubling per attempt up to `max_ns`, each wait
    /// shortened by a random share of up to `jitter_per_mille` thousandths.
    pub const fn new(base_ns: u64, max_ns: u64, jitter_per_mille: u32) -> Reconnect {
        assert!(base_ns > 0 && max_ns >= base_ns && jitter_per_mille <= 1000);
        Reconnect {
            base_ns,
            max_ns,
            jitter_per_mille,
        }
    }
}

/// How one connection behaves; given to `connect`, and to `listen` for the
/// connections it accepts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConnOpts {
    pub(crate) framing: Framing,
    pub(crate) rx: Rx,
    pub(crate) held_max: u32,
    pub(crate) connect_timeout_ns: u64,
    pub(crate) handshake_timeout_ns: u64,
    pub(crate) idle_timeout_ns: u64,
    pub(crate) reconnect: Option<Reconnect>,
    pub(crate) half_close: bool,
    pub(crate) frame_on_packet: bool,
}

impl ConnOpts {
    /// Frames cut by `framing`, read from the buffer ring, up to 4 MiB of
    /// frame memory held before reading pauses, a 10 s connect timeout, no
    /// idle timeout, no reconnecting.
    pub const fn new(framing: Framing) -> ConnOpts {
        ConnOpts {
            framing,
            rx: Rx::Ring,
            held_max: 4 << 20,
            connect_timeout_ns: 10_000_000_000,
            handshake_timeout_ns: 10_000_000_000,
            idle_timeout_ns: 0,
            reconnect: None,
            half_close: false,
            frame_on_packet: false,
        }
    }

    /// With this way of reading.
    #[must_use]
    pub const fn with_rx(mut self, rx: Rx) -> ConnOpts {
        self.rx = rx;
        self
    }

    /// Stop reading while the caller holds this many bytes of the
    /// connection's frames in frame memory; resume when releases bring it
    /// under. At least 1.
    #[must_use]
    pub const fn with_held_max(mut self, bytes: u32) -> ConnOpts {
        assert!(bytes > 0);
        self.held_max = bytes;
        self
    }

    /// Give a connect up after this long; 0 for no limit.
    #[must_use]
    pub const fn with_connect_timeout_ns(mut self, ns: u64) -> ConnOpts {
        self.connect_timeout_ns = ns;
        self
    }

    /// End an accepted connection whose TLS handshake has not finished
    /// after this long; 0 waits for ever. For a connection the caller
    /// opens, the connect timeout covers the handshake too.
    #[must_use]
    pub const fn with_handshake_timeout_ns(mut self, ns: u64) -> ConnOpts {
        self.handshake_timeout_ns = ns;
        self
    }

    /// Close a connection on which nothing was sent or received for this
    /// long; 0 for no limit.
    #[must_use]
    pub const fn with_idle_timeout_ns(mut self, ns: u64) -> ConnOpts {
        self.idle_timeout_ns = ns;
        self
    }

    /// Keep the connection open for sending after its peer has finished
    /// its stream in order: `Event::Finished` says so, and the connection
    /// ends when the caller shuts its own side down or closes it. Without
    /// this the end of the peer's stream ends the connection.
    #[must_use]
    pub const fn with_half_close(mut self, on: bool) -> ConnOpts {
        self.half_close = on;
        self
    }

    /// Send every frame as a send of its own, begun only when the one
    /// before it was taken whole: a frame is then not put into a segment
    /// behind another, and its header and body go out together. This is
    /// for a peer that receives into pages (`Rx::ZeroCopy`), which can
    /// lend a frame where it lies only if the frame starts a packet.
    ///
    /// What it gives is a send a frame, in order, the next one following
    /// when the transport has taken the last, not when the peer has
    /// acknowledged it, and each send marked as ending one of the kernel's
    /// buffers (`MSG_EOR`, Linux), so that the next frame is not joined to
    /// an unsent packet of this one and begins a packet of its own.
    /// Measured between two machines (milestone 31): with packets of two
    /// pages (MTU 8244) and frames of whole 4 KiB blocks, 99.8 to 100 % of
    /// frames then began a page at a zero-copy receiver; a frame of any
    /// other length leaves the next one mid-page, because the receiving
    /// card lays payloads back to back. What it cannot give for certain
    /// is the packet: TCP cuts a packet short when its window says so,
    /// and a send that is taken short ends a buffer in the frame's
    /// middle. A frame that did not start a packet costs its receiver a
    /// copy and nothing else. The backend should have `TCP_NODELAY` on
    /// (`Io::no_delay`), or the kernel holds a frame's last packet back
    /// to join the next frame to it. A frame takes a send segment of its
    /// own, however short it is: size `Sizes::with_segments` for frames,
    /// not for bytes. Under TLS it is a frame's records that go out
    /// together, and nothing is said of packets.
    #[must_use]
    pub const fn with_frame_on_packet(mut self, on: bool) -> ConnOpts {
        self.frame_on_packet = on;
        self
    }

    /// Dial again after the connection is lost or a connect fails, keeping
    /// the handle. For `connect` only.
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: Option<Reconnect>) -> ConnOpts {
        self.reconnect = reconnect;
        self
    }
}

/// How a listener listens and what it does with what it accepts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ListenConfig {
    pub(crate) conn: ConnOpts,
    pub(crate) reuse_port: bool,
    pub(crate) backlog: u32,
    pub(crate) limit: u32,
    pub(crate) peer_addr: bool,
    pub(crate) bind_retry_ns: u64,
    pub(crate) tls: bool,
}

impl ListenConfig {
    /// Accepted connections behave as `conn` says; the port to itself, a
    /// backlog of 128, no connection limit, no peer addresses, no bind retry.
    pub const fn new(conn: ConnOpts) -> ListenConfig {
        ListenConfig {
            conn,
            reuse_port: false,
            backlog: 128,
            limit: u32::MAX,
            peer_addr: false,
            bind_retry_ns: 0,
            tls: false,
        }
    }

    /// Protect accepted connections with TLS: each is announced once its
    /// handshake is done. Needs `Sizes::with_tls` and a transport with a
    /// server setup; in ring mode also frame memory of two buffers at
    /// least, each holding a record. `listen` answers
    /// `NetError::NoTls` or `NetError::Framing` otherwise.
    #[must_use]
    pub const fn with_tls(mut self, on: bool) -> ListenConfig {
        self.tls = on;
        self
    }

    /// Share the port with listeners on other threads (`SO_REUSEPORT`).
    #[must_use]
    pub const fn with_reuse_port(mut self, on: bool) -> ListenConfig {
        self.reuse_port = on;
        self
    }

    /// Let this many connections wait to be accepted.
    #[must_use]
    pub const fn with_backlog(mut self, backlog: u32) -> ListenConfig {
        assert!(backlog > 0);
        self.backlog = backlog;
        self
    }

    /// Hold at most this many connections; one more at a time is accepted
    /// only to be refused, after the caller has had the chance to send it
    /// one frame.
    #[must_use]
    pub const fn with_limit(mut self, connections: u32) -> ListenConfig {
        self.limit = connections;
        self
    }

    /// Report each accepted connection's peer address. Costs one accept
    /// operation per connection in place of one armed accept for all.
    #[must_use]
    pub const fn with_peer_addr(mut self, on: bool) -> ListenConfig {
        self.peer_addr = on;
        self
    }

    /// When the address is in use, keep trying to bind for this long
    /// (a process killed a moment ago may still hold it).
    #[must_use]
    pub const fn with_bind_retry_ns(mut self, ns: u64) -> ListenConfig {
        self.bind_retry_ns = ns;
        self
    }
}
