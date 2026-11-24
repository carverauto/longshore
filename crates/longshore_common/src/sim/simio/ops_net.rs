// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulated socket operations for [`Virtual`], one handler per operation.

use super::*;

impl Virtual {
    pub(super) fn is_udp(&self, sock: Sock) -> bool {
        matches!(
            self.socks[sock.0 as usize].state,
            SockState::UdpBound | SockState::UdpConnected
        )
    }

    pub(super) fn sim_connect(&mut self, sock: Sock, addr: SocketAddr) -> Option<i32> {
        let s = sock.0 as usize;
        if self.socks[s].state == SockState::Open {
            // Accepted. A routed address reports its segment sizes.
            return Some(
                crate::io::Segment {
                    size: self.socks[s].segment,
                    path_limit: self.path_limit,
                }
                .encode(),
            );
        }
        if self.socks[s].state == SockState::Connecting {
            return None;
        }
        let route = self.routes.iter().position(|r| r.addr == addr);
        let (addr, segment) = route.map_or((addr, 0), |k| {
            (self.routes[k].listener, self.routes[k].segment)
        });
        let listening = self
            .socks
            .iter()
            .any(|l| l.state == SockState::Listening && l.addr == Some(SimAddr::Ip(addr)));
        if !listening || self.rng.chance(self.faults.connect_refused) {
            return Some(-ECONNREFUSED);
        }
        self.socks[s].state = SockState::Connecting;
        self.socks[s].addr = Some(SimAddr::Ip(addr));
        self.socks[s].segment = segment;
        if let Some(k) = route {
            self.routes[k].connects += 1;
        }
        None
    }

    pub(super) fn sim_connect_udp(&mut self, sock: Sock, addr: SocketAddr) -> Option<i32> {
        let server = self
            .socks
            .iter()
            .position(|l| l.state == SockState::UdpBound && l.addr == Some(SimAddr::Ip(addr)));
        let s = &mut self.socks[sock.0 as usize];
        s.state = SockState::UdpConnected;
        s.addr = Some(SimAddr::Ip(addr));
        s.has_peer = server.is_some();
        s.peer = server.unwrap_or(0) as u32;
        s.rx_head = 0;
        s.rx_len = 0;
        Some(0)
    }

    pub(super) fn sim_accept(&mut self, listener: Sock, sock: Sock) -> Option<i32> {
        if self.socks[listener.0 as usize].state != SockState::Listening {
            return Some(-EBADF);
        }
        let addr = self.socks[listener.0 as usize].addr;
        let c = self
            .socks
            .iter()
            .position(|c| c.state == SockState::Connecting && c.addr == addr)?
            as u32;
        for (me, peer) in [(c, sock.0), (sock.0, c)] {
            let s = &mut self.socks[me as usize];
            s.state = SockState::Open;
            s.peer = peer;
            s.rx_head = 0;
            s.rx_len = 0;
            s.peer_closed = false;
            (s.peer_wr_shut, s.wr_shut, s.rd_shut) = (false, false, false);
            s.reset = false;
            s.accepted_from = None;
        }
        if let Some(SimAddr::Ip(listening)) = addr {
            // The simulator has no client addresses: the listener's own
            // address and a port derived from the client's slot.
            let port = 49_152 + (c % 16_384) as u16;
            self.socks[sock.0 as usize].accepted_from = Some(SocketAddr::new(listening.ip(), port));
        }
        Some(0)
    }

    /// Sends one datagram to the connected or last-heard peer, if any.
    pub(super) fn sim_udp_send(
        &mut self,
        sock: Sock,
        pool: &[u8],
        limits: &Limits,
        (buf, off, len): (BufId, u32, u32),
    ) -> Option<i32> {
        let s = &self.socks[sock.0 as usize];
        if s.has_peer {
            let to = s.peer as usize;
            let src = limits.range(buf, off, len);
            self.udp_deliver(sock.0, to, pool, src);
        }
        Some(count_result(len))
    }

    /// Receives one datagram; `no_wait` reports `-EAGAIN` instead of waiting.
    pub(super) fn sim_udp_recv(
        &mut self,
        sock: Sock,
        pool: &mut [u8],
        limits: &Limits,
        (buf, off, len): (BufId, u32, u32),
        no_wait: bool,
    ) -> Option<i32> {
        let s = sock.0 as usize;
        if self.socks[s].rx_len == 0 {
            return no_wait.then_some(-EAGAIN);
        }
        let dst = limits.range(buf, off, len);
        let q = &mut self.socks[s];
        let at = |udp: &SimSock, k: usize| udp.rx[(udp.rx_head + k) % SOCK_BUF];
        let n = u16::from_le_bytes([at(q, 0), at(q, 1)]) as usize;
        let from = u32::from(u16::from_le_bytes([at(q, 2), at(q, 3)]));
        let take = n.min(len as usize); // excess is discarded, as with real UDP
        for k in 0..take {
            pool[dst.start + k] = at(q, 4 + k);
        }
        q.rx_head = (q.rx_head + n + 4) % SOCK_BUF;
        q.rx_len -= n + 4;
        if q.state == SockState::UdpBound {
            q.peer = from;
            q.has_peer = true;
        }
        Some(count_result(take))
    }

    pub(super) fn sim_send(
        &mut self,
        sock: Sock,
        pool: &[u8],
        limits: &Limits,
        (buf, off, len): (BufId, u32, u32),
    ) -> Option<i32> {
        let s = sock.0 as usize;
        if self.socks[s].reset {
            return Some(-ECONNRESET);
        }
        if self.socks[s].state != SockState::Open
            || self.socks[s].peer_closed
            || self.socks[s].wr_shut
        {
            return Some(-EPIPE);
        }
        if self.rng.chance(self.faults.reset) {
            self.reset_pair(sock.0);
            return Some(-ECONNRESET);
        }
        let peer = self.socks[s].peer as usize;
        if self.socks[peer].state != SockState::Open || self.socks[peer].peer as usize != s {
            // The far end closed and its slot now belongs to another connection.
            return Some(-EPIPE);
        }
        if self.socks[peer].rd_shut {
            // The far end reads no more: what arrives is dropped.
            return Some(count_result(len));
        }
        let space = SOCK_BUF - self.socks[peer].rx_len;
        if space == 0 || len == 0 {
            return (len == 0).then_some(0);
        }
        let mut n = (len as usize).min(space);
        if self.rng.chance(self.faults.short_send) {
            n = 1 + self.rng.below(n as u64) as usize;
        }
        let src = limits.range(buf, off, n as u32);
        let flip = self
            .rng
            .chance(self.faults.bit_flip)
            .then(|| (self.rng.below(n as u64) as usize, 1u8 << self.rng.below(8)));
        let p = &mut self.socks[peer];
        let tail = (p.rx_head + p.rx_len) % SOCK_BUF;
        for k in 0..n {
            let mut byte = pool[src.start + k];
            if let Some((at, bit)) = flip {
                if at == k {
                    byte ^= bit;
                }
            }
            p.rx[(tail + k) % SOCK_BUF] = byte;
        }
        p.rx_len += n;
        Some(count_result(n))
    }

    /// Receives stream bytes; `no_wait` reports `-EAGAIN` instead of waiting.
    pub(super) fn sim_recv(
        &mut self,
        sock: Sock,
        pool: &mut [u8],
        limits: &Limits,
        (buf, off, len): (BufId, u32, u32),
        no_wait: bool,
    ) -> Option<i32> {
        let s = sock.0 as usize;
        if self.socks[s].reset {
            return Some(-ECONNRESET);
        }
        if self.socks[s].state != SockState::Open {
            return Some(-EPIPE);
        }
        if self.socks[s].rd_shut {
            return Some(0);
        }
        let avail = self.socks[s].rx_len;
        if avail == 0 {
            if self.socks[s].peer_closed || self.socks[s].peer_wr_shut {
                return Some(0);
            }
            // Nothing has arrived: a plain read waits, a read that
            // must not wait says so.
            return no_wait.then_some(-EAGAIN);
        }
        if self.rng.chance(self.faults.reset) {
            self.reset_pair(sock.0);
            return Some(-ECONNRESET);
        }
        let mut n = (len as usize).min(avail);
        if n == 0 {
            return Some(0);
        }
        if self.rng.chance(self.faults.partial_recv) {
            n = 1 + self.rng.below(n as u64) as usize;
        }
        let dst = limits.range(buf, off, n as u32);
        let q = &mut self.socks[s];
        for k in 0..n {
            pool[dst.start + k] = q.rx[(q.rx_head + k) % SOCK_BUF];
        }
        q.rx_head = (q.rx_head + n) % SOCK_BUF;
        q.rx_len -= n;
        Some(count_result(n))
    }

    /// Shuts one or both directions of a connected stream down.
    pub(super) fn sim_shutdown(&mut self, sock: Sock, how: crate::io::Shut) -> Option<i32> {
        use crate::io::Shut;
        let s = sock.0 as usize;
        if self.socks[s].state != SockState::Open || self.is_udp(sock) {
            return Some(-crate::io::ENOTCONN);
        }
        if matches!(how, Shut::Write | Shut::Both) && !self.socks[s].wr_shut {
            self.socks[s].wr_shut = true;
            let peer = self.socks[s].peer as usize;
            if !self.socks[s].peer_closed
                && self.socks[peer].state == SockState::Open
                && self.socks[peer].peer as usize == s
            {
                self.socks[peer].peer_wr_shut = true;
            }
        }
        if matches!(how, Shut::Read | Shut::Both) {
            self.socks[s].rd_shut = true;
            self.socks[s].rx_len = 0;
        }
        Some(0)
    }

    pub(super) fn sim_close(&mut self, sock: Sock) -> Option<i32> {
        let s = sock.0 as usize;
        if self.socks[s].state == SockState::Open {
            let peer = self.socks[s].peer as usize;
            // Only if the peer is still our peer: its slot may have
            // been closed and reused for a new connection since.
            if !self.socks[s].peer_closed
                && self.socks[peer].state == SockState::Open
                && self.socks[peer].peer as usize == s
            {
                self.socks[peer].peer_closed = true;
            }
        }
        self.socks[s].state = SockState::Free;
        self.socks[s].addr = None;
        self.socks[s].accepted_from = None;
        Some(0)
    }
}
