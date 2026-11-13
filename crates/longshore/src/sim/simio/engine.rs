// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What each operation does to the simulated world, and how one step of virtual time runs.

use super::*;

impl SimIo {
    /// Queue one datagram on `to`: [len u16][source socket u16][payload].
    pub(super) fn udp_deliver(&mut self, from: u32, to: usize, src: core::ops::Range<usize>) {
        let n = src.len();
        let lost = self.rng.chance(self.faults.udp_drop) || self.rng.chance(self.faults.bit_flip);
        let q = &mut self.socks[to];
        if lost || n > u16::MAX as usize || SOCK_BUF - q.rx_len < n + 4 {
            return;
        }
        let tail = (q.rx_head + q.rx_len) % SOCK_BUF;
        let (l, f) = ((n as u16).to_le_bytes(), (from as u16).to_le_bytes());
        let head = [l[0], l[1], f[0], f[1]];
        for (k, b) in head.iter().chain(self.bufs[src].iter()).enumerate() {
            q.rx[(tail + k) % SOCK_BUF] = *b;
        }
        q.rx_len += n + 4;
    }

    pub(super) fn reset_pair(&mut self, a: u32) {
        let b = self.socks[a as usize].peer;
        // The far end is reset only if it is still paired with this socket.
        let paired =
            self.socks[b as usize].state == SockState::Open && self.socks[b as usize].peer == a;
        for i in [Some(a), paired.then_some(b)].into_iter().flatten() {
            let s = &mut self.socks[i as usize];
            s.reset = true;
            s.rx_len = 0;
        }
    }

    /// Try to make progress on one operation. `None` means not ready yet.
    pub(super) fn attempt(&mut self, p: &Pending) -> Option<i32> {
        match p.op {
            Op::Timeout { .. } => (self.now >= p.deadline).then_some(0),
            Op::Cancel { .. } => {
                // Mark the operation named at submission, if it is still
                // there; it completes with -ECANCELED on its next visit.
                let target = p.cancel_target;
                match self.pending.iter_mut().find(|q| target == Some(q.id)) {
                    Some(q) => {
                        q.cancelled = true;
                        Some(0)
                    }
                    None => Some(-ENOENT),
                }
            }
            Op::Connect { sock, addr } => {
                let s = sock.0 as usize;
                if self.socks[s].state == SockState::Open {
                    return Some(0); // accepted
                }
                if self.socks[s].state == SockState::Connecting {
                    return None;
                }
                let listening = self
                    .socks
                    .iter()
                    .any(|l| l.state == SockState::Listening && l.addr == Some(addr));
                if !listening || self.rng.chance(self.faults.connect_refused) {
                    return Some(-ECONNREFUSED);
                }
                self.socks[s].state = SockState::Connecting;
                self.socks[s].addr = Some(addr);
                None
            }
            Op::ReadFile {
                file,
                buf,
                off,
                len,
            } => {
                let dst = self.range(buf, off, len);
                match &self.files[file.0 as usize] {
                    None => Some(-ENOENT),
                    Some(data) => {
                        let n = data.len().min(len as usize);
                        self.bufs[dst.start..dst.start + n].copy_from_slice(&data[..n]);
                        Some(n as i32)
                    }
                }
            }
            Op::ConnectUdp { sock, addr } => {
                let server = self
                    .socks
                    .iter()
                    .position(|l| l.state == SockState::UdpBound && l.addr == Some(addr));
                let s = &mut self.socks[sock.0 as usize];
                s.state = SockState::UdpConnected;
                s.addr = Some(addr);
                s.has_peer = server.is_some();
                s.peer = server.unwrap_or(0) as u32;
                s.rx_head = 0;
                s.rx_len = 0;
                Some(0)
            }
            Op::Accept { listener, sock } => {
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
                    s.reset = false;
                }
                Some(0)
            }
            Op::Send {
                sock,
                buf,
                off,
                len,
                ..
            } if matches!(
                self.socks[sock.0 as usize].state,
                SockState::UdpBound | SockState::UdpConnected
            ) =>
            {
                let s = &self.socks[sock.0 as usize];
                if s.has_peer {
                    let to = s.peer as usize;
                    let src = self.range(buf, off, len);
                    self.udp_deliver(sock.0, to, src);
                }
                Some(len as i32)
            }
            Op::Recv {
                sock,
                buf,
                off,
                len,
            } if matches!(
                self.socks[sock.0 as usize].state,
                SockState::UdpBound | SockState::UdpConnected
            ) =>
            {
                let s = sock.0 as usize;
                if self.socks[s].rx_len == 0 {
                    return None;
                }
                let dst = self.range(buf, off, len);
                let q = &mut self.socks[s];
                let at = |q: &SimSock, k: usize| q.rx[(q.rx_head + k) % SOCK_BUF];
                let n = u16::from_le_bytes([at(q, 0), at(q, 1)]) as usize;
                let from = u16::from_le_bytes([at(q, 2), at(q, 3)]) as u32;
                let take = n.min(len as usize); // excess is discarded, as with real UDP
                for k in 0..take {
                    self.bufs[dst.start + k] = at(q, 4 + k);
                }
                q.rx_head = (q.rx_head + n + 4) % SOCK_BUF;
                q.rx_len -= n + 4;
                if q.state == SockState::UdpBound {
                    q.peer = from;
                    q.has_peer = true;
                }
                Some(take as i32)
            }
            Op::Send {
                sock,
                buf,
                off,
                len,
                ..
            } => {
                let s = sock.0 as usize;
                if self.socks[s].reset {
                    return Some(-ECONNRESET);
                }
                if self.socks[s].state != SockState::Open || self.socks[s].peer_closed {
                    return Some(-EPIPE);
                }
                if self.rng.chance(self.faults.reset) {
                    self.reset_pair(sock.0);
                    return Some(-ECONNRESET);
                }
                let peer = self.socks[s].peer as usize;
                if self.socks[peer].state != SockState::Open || self.socks[peer].peer as usize != s
                {
                    // The far end closed and its slot now belongs to another connection.
                    return Some(-EPIPE);
                }
                let space = SOCK_BUF - self.socks[peer].rx_len;
                if space == 0 || len == 0 {
                    return (len == 0).then_some(0);
                }
                let mut n = (len as usize).min(space);
                if self.rng.chance(self.faults.short_send) {
                    n = 1 + self.rng.below(n as u64) as usize;
                }
                let src = self.range(buf, off, n as u32);
                let flip = self
                    .rng
                    .chance(self.faults.bit_flip)
                    .then(|| (self.rng.below(n as u64) as usize, 1u8 << self.rng.below(8)));
                let p = &mut self.socks[peer];
                let tail = (p.rx_head + p.rx_len) % SOCK_BUF;
                for k in 0..n {
                    let mut byte = self.bufs[src.start + k];
                    if let Some((at, bit)) = flip {
                        if at == k {
                            byte ^= bit;
                        }
                    }
                    p.rx[(tail + k) % SOCK_BUF] = byte;
                }
                p.rx_len += n;
                Some(n as i32)
            }
            Op::Recv {
                sock,
                buf,
                off,
                len,
            }
            | Op::RecvNow {
                sock,
                buf,
                off,
                len,
            } => {
                let no_wait = matches!(p.op, Op::RecvNow { .. });
                let s = sock.0 as usize;
                if self.socks[s].reset {
                    return Some(-ECONNRESET);
                }
                if self.socks[s].state != SockState::Open {
                    return Some(-EPIPE);
                }
                let avail = self.socks[s].rx_len;
                if avail == 0 {
                    if self.socks[s].peer_closed {
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
                let dst = self.range(buf, off, n as u32);
                let q = &mut self.socks[s];
                for k in 0..n {
                    self.bufs[dst.start + k] = q.rx[(q.rx_head + k) % SOCK_BUF];
                }
                q.rx_head = (q.rx_head + n) % SOCK_BUF;
                q.rx_len -= n;
                Some(n as i32)
            }
            Op::Close { sock } => {
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
                // A connect still in progress on this socket ends here, even
                // if it was already accepted. Left pending, it would find the
                // socket free and start again.
                for q in self.pending.iter_mut() {
                    if q.attached && matches!(q.op, Op::Connect { sock: c, .. } if c == sock) {
                        q.cancelled = true;
                        q.attached = false;
                    }
                }
                self.socks[s].state = SockState::Free;
                self.socks[s].addr = None;
                Some(0)
            }
        }
    }

    /// One scheduling pass over pending operations, starting at a seeded offset.
    pub(super) fn step(&mut self, out: &mut Completions) {
        self.now += STEP_NS;
        let n = self.pending.len();
        if n == 0 {
            return;
        }
        let start = self.rng.below(n as u64) as usize;
        let mut visited = 0;
        let mut i = start;
        while visited < n && !out.is_full() && !self.pending.is_empty() {
            visited += 1;
            let idx = i % self.pending.len();
            let p = self.pending[idx];
            let ready = if p.cancelled {
                // A connect that was waiting to be accepted gives its slot back.
                if let Op::Connect { sock, .. } = p.op {
                    if p.attached && self.socks[sock.0 as usize].state == SockState::Connecting {
                        self.socks[sock.0 as usize].state = SockState::Free;
                    }
                }
                Some(-ECANCELED)
            } else if self.rng.chance(self.faults.delay) {
                None
            } else {
                self.attempt(&p)
            };
            match ready {
                Some(result) => {
                    self.pending.swap_remove(idx);
                    for v in [p.token.0, result as u32 as u64, self.now] {
                        self.trace = (self.trace ^ v).wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    out.push(Completion {
                        token: p.token,
                        result,
                    });
                    // `swap_remove` moved another entry into `idx`; visit it next.
                }
                None => {
                    if let Op::Connect { sock, .. } = p.op {
                        if self.socks[sock.0 as usize].state == SockState::Connecting {
                            self.pending[idx].attached = true;
                        }
                    }
                    i = idx + 1;
                }
            }
        }
    }
}
