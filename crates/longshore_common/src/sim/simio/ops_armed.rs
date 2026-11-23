// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Armed accept/receive, buffer-ring free list, and Unix-domain connect for [`Virtual`].

use super::*;

impl Virtual {
    pub(super) fn give_back_ring(&mut self, id: BufId) {
        let idx = (id.0 - self.ring_base) as usize;
        assert!(!self.ring_in_free[idx], "ring buffer already in ring");
        self.ring_in_free[idx] = true;
        self.ring_free[self.ring_free_len] = idx as u16;
        self.ring_free_len += 1;
    }

    pub(super) fn attempt_accept_armed(&mut self, listener: Sock) -> Option<Attempt> {
        if self.socks[listener.0 as usize].state != SockState::Listening {
            return Some(Attempt::single(-EBADF));
        }
        let addr = self.socks[listener.0 as usize].addr;
        let c = self
            .socks
            .iter()
            .position(|c| c.state == SockState::Connecting && c.addr == addr)?
            as u32;
        let end = self.accept_start + self.accept_len;
        let slot =
            (self.accept_start..end).find(|&k| self.socks[k as usize].state == SockState::Free);
        if let Some(slot_idx) = slot {
            for (me, peer) in [(c, slot_idx), (slot_idx, c)] {
                let s = &mut self.socks[me as usize];
                s.state = SockState::Open;
                s.peer = peer;
                s.rx_head = 0;
                s.rx_len = 0;
                s.peer_closed = false;
                s.reset = false;
            }
            Some(Attempt {
                result: slot_idx as i32,
                more: true,
                buf: None,
            })
        } else {
            let cs = &mut self.socks[c as usize];
            cs.state = SockState::Open;
            cs.peer = c;
            cs.rx_head = 0;
            cs.rx_len = 0;
            cs.peer_closed = true;
            cs.reset = true;
            Some(Attempt::single(-ENFILE))
        }
    }

    pub(super) fn attempt_recv_armed(
        &mut self,
        sock: Sock,
        pool: &mut [u8],
        limits: &Limits,
    ) -> Option<Attempt> {
        let s = sock.0 as usize;
        if self.socks[s].reset {
            return Some(Attempt::single(-ECONNRESET));
        }
        if self.socks[s].state != SockState::Open {
            return Some(Attempt::single(-EPIPE));
        }
        let avail = self.socks[s].rx_len;
        if avail == 0 {
            if self.socks[s].peer_closed {
                return Some(Attempt::single(0));
            }
            return None;
        }
        if self.rng.chance(self.faults.reset) {
            self.reset_pair(sock.0);
            return Some(Attempt::single(-ECONNRESET));
        }
        if self.ring_free_len == 0 {
            return Some(Attempt::single(-ENOBUFS));
        }
        self.ring_free_len -= 1;
        let ring_idx = self.ring_free[self.ring_free_len];
        self.ring_in_free[ring_idx as usize] = false;
        let buf_id = BufId(self.ring_base + u32::from(ring_idx));
        let mut n = (limits.ring_buffer_size as usize).min(avail);
        if self.rng.chance(self.faults.partial_recv) {
            n = 1 + self.rng.below(n as u64) as usize;
        }
        let dst = limits.range(buf_id, 0, n as u32);
        let q = &mut self.socks[s];
        for k in 0..n {
            pool[dst.start + k] = q.rx[(q.rx_head + k) % SOCK_BUF];
        }
        q.rx_head = (q.rx_head + n) % SOCK_BUF;
        q.rx_len -= n;
        Some(Attempt {
            result: n as i32,
            more: true,
            buf: Some(buf_id),
        })
    }

    pub(super) fn connect_unix(&mut self, sock: Sock, path_with_nul: &[u8]) -> Option<i32> {
        let s = sock.0 as usize;
        if self.socks[s].state == SockState::Open {
            return Some(0);
        }
        if self.socks[s].state == SockState::Connecting {
            return None;
        }
        let mut buf = [0u8; 108];
        buf[..path_with_nul.len()].copy_from_slice(path_with_nul);
        let target = SimAddr::Unix(buf, path_with_nul.len() as u8);
        let listening = self
            .socks
            .iter()
            .any(|l| l.state == SockState::Listening && l.addr == Some(target));
        if !listening {
            return Some(-ENOENT);
        }
        if self.rng.chance(self.faults.connect_refused) {
            return Some(-ECONNREFUSED);
        }
        self.socks[s].state = SockState::Connecting;
        self.socks[s].addr = Some(target);
        self.socks[s].segment = 0;
        None
    }
}
