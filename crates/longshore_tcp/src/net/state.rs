// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What `Net` keeps per connection, listener, ring buffer and queued send.

use crate::addr::Addr;
use crate::backoff::Backoff;
use crate::config::{ConnOpts, ListenConfig};
use crate::event::Why;
use crate::handle::Listener;
use longshore_common::io::extents::Extent;
use longshore_common::io::BufId;
use std::net::{Ipv4Addr, SocketAddr};

pub(super) const NONE: u32 = u32::MAX;

/// For a step that is tried again later, or does not matter, when it fails.
pub(super) fn best_effort<V, E>(_: Result<V, E>) {}

// Events owed to the caller, lowest bit first.
pub(super) const EV_UP: u8 = 1;
pub(super) const EV_ACCEPTED: u8 = 1 << 1;
pub(super) const EV_REFUSED: u8 = 1 << 2;
pub(super) const EV_READABLE: u8 = 1 << 3;
pub(super) const EV_WRITABLE: u8 = 1 << 4;
pub(super) const EV_FINISHED: u8 = 1 << 5;
pub(super) const EV_DOWN: u8 = 1 << 6;
pub(super) const EV_CLOSED: u8 = 1 << 7;

// Work to do on the next `poll`, when the driver is at hand.
pub(super) const W_READ: u8 = 1;
pub(super) const W_FLUSH: u8 = 1 << 1;
pub(super) const W_CLOSE: u8 = 1 << 2;
pub(super) const W_SHUT: u8 = 1 << 3;
pub(super) const W_CONNECT: u8 = 1 << 4;
pub(super) const W_REFUSE: u8 = 1 << 5;
pub(super) const W_ADMIT: u8 = 1 << 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum St {
    Free,
    Connecting,
    /// Accepted by a listener with TLS; the handshake runs. Not yet
    /// announced to the caller.
    Securing,
    Up,
    /// Accepted over the listener's limit: one frame may be sent, no reads.
    Refused,
    /// The stream has ended and input is left to pull. No end is owed
    /// until it has been pulled.
    Finishing,
    /// The peer finished in order and everything was pulled; with
    /// half-close, sending goes on until the caller shuts down or closes.
    HalfClosed,
    /// Ended, with nothing left to pull: the end is owed to the caller.
    Dying,
    /// Being closed: waiting for the close and for operations in flight.
    Closing,
    /// Waiting to dial again.
    Backoff,
}

/// The socket of a connection slot. A slot is freed only when its socket
/// is `Closed`, or it never had one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Socket {
    /// None: not connected yet, or the connect failed.
    None,
    /// Open, or taken to be: its close has not been handed over yet.
    Open,
    /// Its close is with the transport.
    Closing,
    /// Its close has completed, or there was nothing to close.
    Closed,
}

/// What keeps a queued send's bytes alive.
#[derive(Clone, Copy)]
pub(super) enum Keep {
    /// A send segment, by index, returned to the pool when sent.
    Segment(u32),
    /// A hold on frame memory, released when sent.
    Extent(Extent),
}

/// One queued send.
#[derive(Clone, Copy)]
pub(super) struct TxSpan {
    pub(super) buf: BufId,
    pub(super) off: u32,
    pub(super) len: u32,
    pub(super) keep: Keep,
}

/// A ring buffer with received bytes not yet all pulled.
#[derive(Clone, Copy)]
pub(super) struct Chunk {
    pub(super) off: u32,
    pub(super) len: u32,
    /// Next chunk of the same connection, or `NONE`.
    pub(super) next: u32,
}

#[derive(Clone, Copy)]
pub(super) struct ConnState {
    pub(super) st: St,
    pub(super) opts: ConnOpts,
    /// The listener that accepted it, by the handle its caller knows it
    /// by: the slot may hold another listener by the time the connection
    /// is announced or freed. `Listener::NONE` for one the caller opened.
    pub(super) accepted_by: Listener,
    /// The handle of the listener whose connection is parked here.
    pub(super) parked_by: Option<Listener>,
    /// Where to dial again.
    pub(super) peer: Option<Addr>,
    /// A Unix-domain socket: never sent from without a copy.
    pub(super) unix: bool,
    pub(super) backoff: Backoff,
    pub(super) last_active: u64,
    pub(super) ev: u8,
    pub(super) queued: bool,
    pub(super) want: u8,
    pub(super) working: bool,
    pub(super) why: Why,
    /// The socket in the slot, and how far its close has come.
    pub(super) sock: Socket,
    pub(super) redial: bool,
    /// Its armed receive ended because the ring had no buffer.
    pub(super) dry: bool,
    /// A cancel of its armed receive is with the transport: one is
    /// enough, however often reading is stopped before it lands.
    pub(super) cancelling: bool,
    /// The listener whose next connection the kernel has already put in
    /// this slot, while operations of the last one are still in flight; or
    /// `NONE`. It becomes a connection once the slot is free.
    pub(super) parked: u32,
    /// Counted among its listener's connections.
    pub(super) counted: bool,
    pub(super) connecting: bool,
    /// Protected with TLS.
    pub(super) secure: bool,
    /// A handshake is in flight in the transport.
    pub(super) securing: bool,
    /// A shutdown of the write side is in flight in the transport.
    pub(super) shutting: bool,
    // Receiving.
    pub(super) armed: bool,
    pub(super) recv_busy: bool,
    pub(super) paused: bool,
    /// Protected: the transport holds plaintext to ask for with no bytes:
    /// a whole record that found no room, or what arrived behind the
    /// handshake.
    pub(super) unopened: bool,
    /// Protected: the stream ended before a close alert was opened. The
    /// connection stays up, reading no more, until the records received
    /// have been opened: one of them may be the alert.
    pub(super) cut_off: bool,
    pub(super) hdr_have: u32,
    /// A frame being put together in frame memory.
    pub(super) asm: Option<Extent>,
    pub(super) asm_have: u32,
    pub(super) asm_len: u32,
    /// A whole frame in frame memory, waiting to be pulled.
    pub(super) ready: Option<(Extent, u32)>,
    /// Bytes moved out of ring buffers the caller did not pull in time.
    pub(super) backlog: Option<Extent>,
    pub(super) backlog_off: u32,
    pub(super) backlog_len: u32,
    /// Listed among the connections with ring buffers not yet pulled.
    pub(super) ring_listed: bool,
    pub(super) chunk_head: u32,
    pub(super) chunk_tail: u32,
    /// Bytes of frame memory the caller holds through delivered frames.
    pub(super) held: u32,
    // Sending.
    pub(super) tx_head: u32,
    pub(super) tx_len: u32,
    pub(super) sending: bool,
    pub(super) full: bool,
    /// Listed among the connections waiting for a send segment.
    pub(super) seg_listed: bool,
    pub(super) shut: bool,
    /// A refused connection has been sent its one frame.
    pub(super) answered: bool,
    /// The shutdown has been handed to the transport.
    pub(super) shut_sent: bool,
}

impl ConnState {
    pub(super) fn free(opts: ConnOpts, seed: u64) -> ConnState {
        ConnState {
            st: St::Free,
            opts,
            peer: None,
            unix: false,
            backoff: Backoff::new(1, 1, 0, seed),
            last_active: 0,
            ev: 0,
            queued: false,
            want: 0,
            working: false,
            why: Why::Reset,
            sock: Socket::None,
            redial: false,
            dry: false,
            cancelling: false,
            parked: NONE,
            parked_by: None,
            accepted_by: Listener::NONE,
            counted: false,
            connecting: false,
            secure: false,
            securing: false,
            shutting: false,
            armed: false,
            recv_busy: false,
            paused: false,
            unopened: false,
            cut_off: false,
            hdr_have: 0,
            asm: None,
            asm_have: 0,
            asm_len: 0,
            ready: None,
            backlog: None,
            backlog_off: 0,
            backlog_len: 0,
            ring_listed: false,
            chunk_head: NONE,
            chunk_tail: NONE,
            held: 0,
            tx_head: 0,
            tx_len: 0,
            sending: false,
            full: false,
            seg_listed: false,
            shut: false,
            answered: false,
            shut_sent: false,
        }
    }

    /// Start the slot over for a new connection. Its places in the `Net`'s
    /// lists are not the connection's, and stay.
    pub(super) fn renew(&mut self, opts: ConnOpts, seed: u64) {
        *self = ConnState {
            queued: self.queued,
            working: self.working,
            seg_listed: self.seg_listed,
            ..ConnState::free(opts, seed)
        };
    }

    /// Whether the bytes received so far end inside a frame.
    pub(super) fn mid_frame(&self) -> bool {
        self.hdr_have > 0 || (self.asm.is_some() && self.asm_have > 0)
    }

    /// Whether an operation of this connection is still with the transport.
    pub(super) fn in_flight(&self) -> bool {
        self.sending
            || self.recv_busy
            || self.armed
            || self.connecting
            || self.securing
            || self.shutting
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LSt {
    Free,
    /// The address was in use; trying again until `bind_until`.
    Binding,
    Listening,
    Closing,
}

#[derive(Clone, Copy)]
pub(super) struct ListenerState {
    pub(super) st: LSt,
    pub(super) cfg: ListenConfig,
    pub(super) addr: Addr,
    pub(super) bound: Option<SocketAddr>,
    /// Connections it accepted that are still open, refused ones aside.
    pub(super) count: u32,
    pub(super) armed: bool,
    /// The slot a single accept in flight will fill.
    pub(super) single: Option<u32>,
    /// The wait after the last failed accept; 0 when the last one worked.
    pub(super) backoff_ns: u64,
    /// An accept failed and its wait is not over.
    pub(super) waiting: bool,
    /// Its close is with the transport.
    pub(super) close_sent: bool,
    /// Its close has completed; an accept may still have to report.
    pub(super) close_done: bool,
    pub(super) bind_until: u64,
    /// A failed bind to report, as a negated error number.
    pub(super) failed: Option<i32>,
}

impl ListenerState {
    /// A listener slot nothing listens in.
    pub(super) fn unused(opts: ConnOpts) -> ListenerState {
        ListenerState {
            st: LSt::Free,
            cfg: ListenConfig::new(opts),
            addr: Addr::Ip(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))),
            bound: None,
            count: 0,
            armed: false,
            single: None,
            backoff_ns: 0,
            waiting: false,
            close_sent: false,
            close_done: false,
            bind_until: 0,
            failed: None,
        }
    }
}
