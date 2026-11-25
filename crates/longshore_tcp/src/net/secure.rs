// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS, as far as `Net` sees it: who is protected, when the handshake
//! runs and how long it may take, and where the plaintext of an armed
//! receive goes. The records themselves are the transport's business.

use super::cover::Col;
use super::state::{best_effort, St, EV_READABLE, W_READ};
use super::Net;
use crate::addr::{Addr, ServerName};
use crate::config::{ConnOpts, Rx};
use crate::event::{NetError, Why};
use crate::handle::Conn;
use crate::transport::{Refused, Role, Span, Transport};
use longshore_stream::RING_DST_MIN;

impl<T: Transport> Net<T> {
    /// Open a connection to `addr` protected with TLS; the peer's
    /// certificate must match `server_name`. [`crate::Event::Up`] follows
    /// when the handshake is done; the connect timeout covers both.
    pub fn connect_tls(
        &mut self,
        d: &mut T::Driver,
        addr: impl Into<Addr>,
        server_name: &str,
        opts: ConnOpts,
    ) -> Result<Conn, NetError> {
        self.fits_tls(&opts, true)?;
        let name = ServerName::new(server_name).ok_or(NetError::NoTls)?;
        self.connect_as(d, addr.into(), opts, Some(name))
    }

    /// Whether a connection with `opts` can be protected on the connecting
    /// side (`client`) or the accepting side.
    pub(super) fn fits_tls(&self, opts: &ConnOpts, client: bool) -> Result<(), NetError> {
        if !self.cfg.sizes.tls || !self.t.can_secure(client) {
            return Err(NetError::NoTls);
        }
        // A record is opened whole, into frame memory, and a frame that
        // spans records needs a second buffer while it is put together.
        let sizes = &self.cfg.sizes;
        if opts.rx == Rx::Ring && (sizes.frame_size < RING_DST_MIN || sizes.frames < 2) {
            return Err(NetError::Framing);
        }
        Ok(())
    }

    /// Start the handshake of the connected `slot`.
    pub(super) fn begin_secure(&mut self, d: &mut T::Driver, slot: u32, client: bool) {
        let role = match self.names.get(slot as usize).filter(|_| client) {
            Some(name) => Role::Client {
                server_name: name.as_str(),
            },
            None => Role::Server,
        };
        if self.t.secure(d, slot, role).is_ok() {
            self.conns[slot as usize].securing = true;
        } else {
            self.end(slot, Why::Tls);
        }
    }

    /// The handshake of `slot` ended.
    pub(super) fn on_secured(&mut self, d: &mut T::Driver, slot: u32, result: i32) {
        self.cover(slot, Col::Secured);
        let c = &mut self.conns[slot as usize];
        c.securing = false;
        match c.st {
            St::Connecting if result >= 0 => self.connected(d, slot),
            St::Securing if result >= 0 => self.admit(d, slot),
            St::Connecting | St::Securing => return self.end(slot, Why::of(result)),
            // Timed out or closed meanwhile.
            St::Free
            | St::Up
            | St::Refused
            | St::Finishing
            | St::HalfClosed
            | St::Dying
            | St::Closing
            | St::Backoff => {
                return self.try_free(slot);
            }
        }
        let up = &mut self.conns[slot as usize];
        if up.st == St::Up && up.opts.rx == Rx::Ring {
            // What the peer sent right behind its handshake is with the
            // stream, to be asked for like a record it keeps.
            up.unopened = true;
            if self.open_ring(d, slot) {
                self.queue_event(slot, EV_READABLE);
            }
        }
    }

    /// Open the records in the ring buffers of the protected `slot` into
    /// its backlog, as far as frame memory has room. What the transport
    /// itself holds (a record kept for lack of room, or what arrived
    /// behind the handshake) is asked for with no bytes when there are
    /// none, and again from `poll` while there is no frame memory for it.
    /// Buffers that are done go back on the next `poll`. `true` when there
    /// is plaintext to pull.
    pub(super) fn open_ring(&mut self, d: &mut T::Driver, slot: u32) -> bool {
        let size = self.cfg.sizes.frame_size;
        while self.conns[slot as usize].st == St::Up {
            let chunk = self.front_chunk(slot);
            let c = &mut self.conns[slot as usize];
            let kept = core::mem::take(&mut c.unopened);
            let from = match chunk {
                Some(src) => Span::new(src.buf, src.off, src.avail),
                None if kept => Span::new(self.cfg.header_first, 0, 0),
                None => break,
            };
            let ext = match c.backlog {
                Some(ext) => ext,
                None => {
                    // A protected connection can need two buffers: the
                    // frame it is putting together, and the records that
                    // hold the rest of it. The last free one is kept for
                    // a connection that holds one already, or those that
                    // do could all wait for a second for ever.
                    let first = c.asm.is_none() && c.ready.is_none();
                    let spare = usize::from(first);
                    let taken = (self.extents.free_count() > spare)
                        .then(|| self.extents.take())
                        .flatten();
                    let Some(ext) = taken else {
                        if kept {
                            // Asked for again when frame memory is free.
                            c.unopened = true;
                            self.add_work(slot, W_READ);
                        }
                        break;
                    };
                    (c.backlog, c.backlog_off, c.backlog_len) = (Some(ext), 0, 0);
                    ext
                }
            };
            let have = c.backlog_len;
            let into = Span::new(ext.buf(), have, size - have);
            let Some(opened) = self.t.open(d, slot, from, into) else {
                self.drop_chunks(slot);
                self.end(slot, Why::Tls);
                break;
            };
            self.conns[slot as usize].backlog_len += opened.produced();
            if let Some(src) = chunk {
                self.advance(slot, src, opened.consumed());
            }
            if opened.closed() {
                self.drop_chunks(slot);
                self.peer_finished(slot);
                break;
            }
            if opened.no_room() {
                self.conns[slot as usize].unopened = true;
                if !self.make_room(slot, have, opened.produced()) {
                    break;
                }
            }
        }
        self.end_if_opened(slot);
        let c = &self.conns[slot as usize];
        if c.st == St::Up && c.dry && !c.unopened && !c.armed && !c.recv_busy {
            // Nothing is left with the stream, and the ring had no buffer
            // for this connection: it reads on from `poll`.
            self.add_work(slot, W_READ);
        }
        self.trim_backlog(slot)
    }

    /// The ring had no buffer for the protected `slot`: receive into its
    /// stream's own memory instead, once.
    pub(super) fn read_kept(&mut self, d: &mut T::Driver, slot: u32) {
        if self.conns[slot as usize].unopened {
            // What the stream holds is opened first: a receive would be
            // written behind it, and it could not be opened meanwhile.
            // `poll` asks again while it waits for frame memory.
            return;
        }
        match self.t.recv_kept(d, slot) {
            Ok(true) => {
                let c = &mut self.conns[slot as usize];
                (c.recv_busy, c.dry) = (true, false);
            }
            // A whole record waits there for frame memory.
            Ok(false) => self.conns[slot as usize].unopened = true,
            Err(Refused::Busy) => self.add_work(slot, W_READ),
            Err(Refused::Unsupported) => {
                // A transport without memory of its own for this: back
                // to the ring, and to waiting for a buffer of it.
                self.conns[slot as usize].dry = false;
                self.add_work(slot, W_READ);
            }
        }
    }

    /// The receive [`Net::read_kept`] started ended.
    pub(super) fn on_kept(&mut self, d: &mut T::Driver, slot: u32, result: i32) {
        let Ok(n) = u32::try_from(result) else {
            return self.end(slot, Why::of(result));
        };
        let c = &mut self.conns[slot as usize];
        if n == 0 {
            // As when an armed receive finds the end of the stream.
            c.cut_off = true;
            return self.end_if_opened(slot);
        }
        (c.last_active, c.unopened) = (self.now, true);
        if self.open_ring(d, slot) {
            self.queue_event(slot, EV_READABLE);
        }
        // Back to the ring, or here again while it stays empty.
        self.add_work(slot, W_READ);
    }

    /// The stream of `slot` ended without a close alert: it is a broken
    /// connection once every record received has been opened and none
    /// was one. Until then it stays up, with its ring buffers.
    pub(super) fn end_if_opened(&mut self, slot: u32) {
        let c = &self.conns[slot as usize];
        if c.st == St::Up && c.cut_off && !c.unopened && self.front_chunk(slot).is_none() {
            self.end(slot, Why::Reset);
        }
    }

    /// The backlog of `slot` has no room for the next record. `true` when
    /// the next round gets a new one: everything in this one is pulled.
    fn make_room(&mut self, slot: u32, had: u32, produced: u32) -> bool {
        let c = &self.conns[slot as usize];
        if c.backlog_off < c.backlog_len {
            // The caller pulls first.
            return false;
        }
        if had == 0 && produced == 0 {
            // Not even empty frame memory holds this record.
            self.drop_chunks(slot);
            self.end(slot, Why::Protocol);
            return false;
        }
        // Frames lent from the full one keep it; the next record gets a
        // backlog of its own.
        let _nothing_to_pull: bool = self.trim_backlog(slot);
        true
    }

    /// Let go of a backlog with nothing left to pull; `true` when there is
    /// something.
    fn trim_backlog(&mut self, slot: u32) -> bool {
        let c = &mut self.conns[slot as usize];
        if c.backlog_off < c.backlog_len {
            return true;
        }
        if let Some(ext) = c.backlog.take() {
            (c.backlog_off, c.backlog_len) = (0, 0);
            // Frames lent from it keep their own holds.
            best_effort(self.extents.release(ext));
        }
        false
    }
}
