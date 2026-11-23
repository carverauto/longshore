// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Simulated socket and file operations for [`Virtual`].

use super::*;

impl Virtual {
    /// Queue one datagram on `to`: [len u16][source socket u16][payload].
    pub(super) fn udp_deliver(
        &mut self,
        from: u32,
        to: usize,
        pool: &[u8],
        src: core::ops::Range<usize>,
    ) {
        let n = src.len();
        let lost = self.rng.chance(self.faults.udp_drop) || self.rng.chance(self.faults.bit_flip);
        let q = &mut self.socks[to];
        if lost || n > u16::MAX as usize || SOCK_BUF - q.rx_len < n + 4 {
            return;
        }
        let tail = (q.rx_head + q.rx_len) % SOCK_BUF;
        let (l, f) = ((n as u16).to_le_bytes(), (from as u16).to_le_bytes());
        let head = [l[0], l[1], f[0], f[1]];
        for (k, b) in head.iter().chain(pool[src].iter()).enumerate() {
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
    pub(super) fn attempt_op(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        file_op_num: u64,
    ) -> Option<i32> {
        match *op {
            Op::Timeout { .. } | Op::AwaitWake | Op::Cancel { .. } => {
                unreachable!("resolved by Engine")
            }
            Op::Connect { sock, addr } => self.sim_connect(sock, addr),
            Op::ReadFile {
                file,
                buf,
                off,
                len,
            } => self.sim_read_file(file, pool, limits.range(buf, off, len), len),
            Op::ConnectUdp { sock, addr } => self.sim_connect_udp(sock, addr),
            Op::Accept { listener, sock } => self.sim_accept(listener, sock),
            Op::Send {
                sock,
                buf,
                off,
                len,
                ..
            } => {
                let span = (buf, off, len);
                if self.is_udp(sock) {
                    self.sim_udp_send(sock, pool, limits, span)
                } else {
                    self.sim_send(sock, pool, limits, span)
                }
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
                let (span, no_wait) = ((buf, off, len), matches!(op, Op::RecvNow { .. }));
                if self.is_udp(sock) {
                    self.sim_udp_recv(sock, pool, limits, span, no_wait)
                } else {
                    self.sim_recv(sock, pool, limits, span, no_wait)
                }
            }
            Op::Close { sock } => self.sim_close(sock),
            Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => Some(self.sim_file_op(op, pool, limits, file_op_num)),
            Op::ConnectUnix { .. } | Op::ArmedAccept { .. } | Op::ArmedRecv { .. } => {
                unreachable!("handled above")
            }
        }
    }

    /// Runs a file operation on the simulated disk; `-ENOENT` without one.
    fn sim_file_op(&mut self, op: &Op, pool: &mut [u8], limits: &Limits, file_op_num: u64) -> i32 {
        match self.disk.as_mut() {
            Some(disk) => {
                let mut ctx = disk::DiskFaultCtx {
                    faults: &self.disk_faults,
                    placed: &mut self.placed_faults,
                    counters: &mut self.disk_counters,
                    rng: &mut self.rng,
                };
                disk.attempt(op, pool, limits, file_op_num, &mut ctx)
            }
            None => -ENOENT,
        }
    }

    fn sim_read_file(
        &self,
        file: FileId,
        pool: &mut [u8],
        dst: core::ops::Range<usize>,
        len: u32,
    ) -> Option<i32> {
        match &self.files[file.0 as usize] {
            None => Some(-ENOENT),
            Some(data) => {
                let n = data.len().min(len as usize);
                pool[dst.start..dst.start + n].copy_from_slice(&data[..n]);
                Some(count_result(n))
            }
        }
    }
}
