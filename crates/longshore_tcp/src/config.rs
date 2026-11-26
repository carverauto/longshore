// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a `Net` is built with: how many connections and listeners, and
//! which socket slots, tokens and buffers of the shared backend are its own.
//!
//! The backend owns one buffer pool, sized by `Limits`. A `Net` allocates no
//! buffer: [`Config::plan`] adds what it needs to the `Limits` the owner
//! already has for files and other clients, and records which ranges of the
//! result belong to the `Net`.

use crate::framing::Framing;
pub use crate::sizes::{Sizes, TLS_SEGMENT_MIN};

mod error;
mod zc;

pub use error::ConfigError;
use longshore_common::io::{BufId, Limits};

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
    /// acknowledged it. What it cannot give is the packet: TCP joins the
    /// first bytes of a send to an unsent packet that has room, joins
    /// packets it sends again, and cuts a packet short when its window
    /// says so. A frame that did not start a packet costs its receiver a
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

/// A `Net`'s share of the backend: built by [`Config::plan`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub(crate) sizes: Sizes,
    /// Socket slot of listener 0; connections follow the listeners.
    pub(crate) sock_base: u32,
    /// Position of the first frame buffer in the block-aligned class, and
    /// of the first send segment in the small class; header areas follow
    /// the segments. Positions, because buffer ids move when something
    /// planned later adds buffers to an earlier class.
    pub(crate) aligned_at: u32,
    pub(crate) small_at: u32,
    /// Connections whose header areas fit in one small buffer.
    pub(crate) headers_per_buf: u32,
    // Buffer ids, set by `resolved` from the backend's final `Limits`.
    pub(crate) frame_first: BufId,
    pub(crate) segment_first: BufId,
    pub(crate) header_first: BufId,
    /// With `Sizes::with_tls`: connection `i` has buffers `2 * i` and
    /// `2 * i + 1` from here.
    pub(crate) tls_first: BufId,
    pub(crate) ring_first: BufId,
    pub(crate) ring_count: u32,
    /// Position of the first buffer of tail blocks in the block-aligned
    /// class, behind the frame buffers, and its id; how many buffers.
    pub(crate) tail_at: u32,
    pub(crate) tail_first: BufId,
    pub(crate) tail_bufs: u32,
    /// The backend's receive area: its first page and how many it has.
    pub(crate) area_first: BufId,
    pub(crate) area_count: u32,
}

impl Config {
    /// What `plan` refuses, and two numbers it needs: connections in all,
    /// and header areas per small buffer.
    fn check(base: &Limits, s: &Sizes) -> Result<(u32, u32), ConfigError> {
        let conns = s
            .connects
            .checked_add(s.accepts)
            .ok_or(ConfigError::TooSmall)?;
        if conns == 0 || s.frames == 0 || s.segments == 0 || s.spans == 0 || s.header_max == 0 {
            return Err(ConfigError::Zero);
        }
        if s.accepts > 0 && base.accept_range_len() != 0 {
            return Err(ConfigError::AcceptRangeTaken);
        }
        let block = base.block_size().max(4096);
        if s.frame_size == 0 || !s.frame_size.is_multiple_of(block) {
            return Err(ConfigError::FrameAlignment);
        }
        let taken = |count: u32, size: u32, want: u32| count != 0 && size != want;
        if taken(
            base.aligned_buffers(),
            base.aligned_buffer_size(),
            s.frame_size,
        ) || taken(
            base.small_buffers(),
            base.small_buffer_size(),
            s.segment_size,
        ) || taken(
            base.ring_buffers(),
            base.ring_buffer_size(),
            s.ring_buffer_size,
        ) {
            return Err(ConfigError::ClassSize);
        }
        let headers_per_buf = s.segment_size / s.header_max;
        if headers_per_buf == 0 || (s.tls && s.segment_size < TLS_SEGMENT_MIN) {
            return Err(ConfigError::TooSmall);
        }
        zc::check(s, conns)?;
        Ok((conns, headers_per_buf))
    }

    /// Add what `sizes` asks for to `base` and say which part is the
    /// `Net`'s. Build the backend from the returned `Limits`, or from
    /// `Limits` planned further on top of them, and give those to
    /// `Net::new`.
    pub fn plan(base: Limits, sizes: Sizes) -> Result<(Limits, Config), ConfigError> {
        let s = sizes;
        let (conns, headers_per_buf) = Config::check(&base, &s)?;
        let header_bufs = conns.div_ceil(headers_per_buf);
        // With TLS, two buffers per connection follow the header areas.
        let tls_bufs = if s.tls { conns.saturating_mul(2) } else { 0 };
        let small = s
            .segments
            .checked_add(header_bufs)
            .and_then(|n| n.checked_add(tls_bufs))
            .ok_or(ConfigError::TooSmall)?;
        let sock_base = base.sockets_max();
        let socks = conns
            .checked_add(s.listeners)
            .ok_or(ConfigError::TooSmall)?;
        // Per connection: a receive, a send, and a connect, close, shutdown
        // or cancel; per listener: an accept and a close; and the
        // zero-copy sends whose memory is still read.
        let ops = conns
            .saturating_mul(3)
            .saturating_add(s.listeners.saturating_mul(2))
            .saturating_add(s.lent_most(conns));
        let mut limits = base
            .with_sockets_max(sock_base + socks)
            .with_block_size(base.block_size().max(4096))
            .with_aligned_buffers(base.aligned_buffers() + s.frames + zc::tail_bufs(&s))
            .with_aligned_buffer_size(s.frame_size)
            .with_small_buffers(base.small_buffers() + small)
            .with_small_buffer_size(s.segment_size)
            .with_ring_buffers(base.ring_buffers().max(s.ring_buffers))
            .with_ring_buffer_size(s.ring_buffer_size)
            .with_ops_max(base.ops_max().saturating_add(ops));
        limits = zc::area(limits, &s, conns);
        if s.accepts > 0 {
            limits = limits
                .with_accept_range_start(sock_base + s.listeners + s.connects)
                .with_accept_range_len(s.accepts);
        }
        let config = Config {
            sizes,
            sock_base,
            aligned_at: base.aligned_buffers(),
            small_at: base.small_buffers(),
            headers_per_buf,
            frame_first: BufId::new(0),
            segment_first: BufId::new(0),
            header_first: BufId::new(0),
            tls_first: BufId::new(0),
            ring_first: BufId::new(0),
            ring_count: 0,
            tail_at: base.aligned_buffers() + s.frames,
            tail_first: BufId::new(0),
            tail_bufs: zc::tail_bufs(&s),
            area_first: BufId::new(0),
            area_count: 0,
        }
        .resolved(&limits);
        Ok((limits, config))
    }

    /// Connections in all: those the `Net` opens and those it accepts.
    pub const fn conns(&self) -> u32 {
        self.sizes.connects + self.sizes.accepts
    }

    /// Tokens the `Net` uses, from `Sizes::with_token_base` up: route a
    /// completion with a token in that range to `Net::on_completion`.
    pub const fn tokens(&self) -> u64 {
        // A multiple of eight, so that the next set's tokens say their
        // kind as this one's do.
        let lent = self.conns() as u64 * self.sizes.zero_copy_sends as u64;
        self.lent_tokens() + lent.next_multiple_of(8)
    }

    /// How far above the first token those of the zero-copy sends
    /// reported twice start: `Sizes::with_zero_copy_sends` a connection,
    /// behind the eight of every connection and listener.
    pub(crate) const fn lent_tokens(&self) -> u64 {
        (self.conns() as u64 + self.sizes.listeners as u64) * 8
    }

    /// The first token.
    pub const fn token_base(&self) -> u64 {
        self.sizes.token_base
    }

    /// This configuration with its buffer ids taken from `limits`: the
    /// `Limits` the backend was finally built with, which may have grown
    /// since `plan` (another `plan` on top, more buffers for files).
    #[must_use]
    pub fn resolved(mut self, limits: &Limits) -> Config {
        self.frame_first = limits.aligned(self.aligned_at);
        self.segment_first = limits.small(self.small_at);
        self.header_first = limits.small(self.small_at + self.sizes.segments);
        if self.sizes.tls {
            let header_bufs = self.conns().div_ceil(self.headers_per_buf);
            self.tls_first = limits.small(self.small_at + self.sizes.segments + header_bufs);
        }
        self.ring_first = BufId::new(limits.ring_base());
        self.ring_count = limits.ring_buffers();
        if self.tail_bufs > 0 {
            self.tail_first = limits.aligned(self.tail_at);
        }
        self.area_first = BufId::new(limits.area_base());
        self.area_count = limits.area_pages();
        self
    }

    /// The first buffer of frame memory, in the `Limits` this was last
    /// resolved against; `Sizes::with_frames` gave their count and size.
    pub const fn frame_first(&self) -> BufId {
        self.frame_first
    }
}
