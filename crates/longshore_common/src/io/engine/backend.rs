// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `Io` trait implementation for `Engine<W>`.

use super::*;
use crate::io::{BufId, Busy, Io};

impl<W: World> Io for Engine<W> {
    fn now_ns(&self) -> u64 {
        self.world.now_ns()
    }

    fn unix_secs(&self) -> u64 {
        self.world.unix_secs()
    }

    fn buf(&mut self, id: BufId) -> &mut [u8] {
        let (at, len) = self.limits.span(id);
        &mut self.bufs[self.pool_base + at..self.pool_base + at + len]
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.limits.pair(&mut self.bufs[self.pool_base..], a, b)
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.world.register_file(path)
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.world.listen(sock, addr)
    }

    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        self.world.listen_unix(sock, path)
    }

    fn no_delay(&mut self, on: bool) {
        self.world.no_delay(on);
    }

    fn keep_alive(&mut self, on: bool) {
        self.world.keep_alive(on);
    }

    fn give_back(&mut self, id: BufId) {
        assert!(
            self.limits.is_ring_buf(id),
            "give_back requires a ring buffer"
        );
        self.world.give_back(id);
    }

    fn measure_segment_sizes(&mut self, on: bool) {
        self.world.measure_segment_sizes(on);
    }

    fn waker(&mut self) -> Option<Arc<dyn Wake>> {
        self.world.waker(self.woken.clone())
    }

    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        if self.pending.len() + self.ready.len() >= self.limits.ops_max as usize {
            return Err(Busy);
        }
        if op.is_file_op() {
            let pool = &self.bufs[self.pool_base..];
            if let Err(errno) = self.files.check_op(&op, pool, &self.limits) {
                self.ready.push(Completion::new(token, errno));
                return Ok(());
            }
        }
        let (unix_path, unix_len) = match op {
            Op::ArmedAccept { listener } => {
                assert!(
                    listener.0 < self.limits.sockets_max,
                    "socket slot out of range"
                );
                if self.limits.accept_range_len == 0 {
                    self.ready.push(Completion::new(token, -ENFILE));
                    return Ok(());
                }
                ([0u8; 108], 0)
            }
            Op::ArmedRecv { sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                if self.limits.ring_buffers == 0 {
                    self.ready.push(Completion::new(token, -ENOBUFS));
                    return Ok(());
                }
                ([0u8; 108], 0)
            }
            Op::ConnectUnix {
                sock,
                path_buf,
                path_off,
                path_len,
            } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let pool = &self.bufs[self.pool_base..];
                match crate::io::files::check_unix_path(
                    pool,
                    &self.limits,
                    path_buf,
                    path_off,
                    path_len,
                    108,
                ) {
                    Ok(slice) => {
                        let mut buf = [0u8; 108];
                        buf[..slice.len()].copy_from_slice(slice);
                        (buf, slice.len() as u8)
                    }
                    Err(errno) => {
                        self.ready.push(Completion::new(token, errno));
                        return Ok(());
                    }
                }
            }
            Op::Connect { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => ([0u8; 108], 0),
        };
        let deadline = match op {
            Op::Timeout { nanos } => self.world.now_ns().saturating_add(nanos),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => 0,
        };
        let cancel_target = match op {
            Op::Cancel { target } => self
                .pending
                .iter()
                .find(|q| {
                    q.token == target
                        && (q.chain_id == 0 || q.chain_active)
                        && !matches!(
                            q.op,
                            Op::Cancel { .. } | Op::Close { .. } | Op::FileClose { .. }
                        )
                })
                .map(|q| q.id),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => None,
        };
        self.next_op_id += 1;
        let world_tag = self.world.on_submit(&op);
        self.pending.push(Pending {
            token,
            op,
            deadline,
            cancelled: false,
            id: self.next_op_id,
            cancel_target,
            attached: false,
            fresh: true,
            chain_id: 0,
            chain_pos: 0,
            chain_active: true,
            chain_aborted: false,
            world_tag,
            last_delivered_step: 0,
            unix_path,
            unix_len,
        });
        Ok(())
    }

    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), Busy> {
        if ops.is_empty() {
            return Ok(());
        }
        if self.pending.len() + self.ready.len() + ops.len() > self.limits.ops_max as usize {
            return Err(Busy);
        }
        let pool = &self.bufs[self.pool_base..];
        if let Err((bad_idx, errno)) = self.files.check_chain(ops, pool, &self.limits) {
            for (idx, &(token, _)) in ops.iter().enumerate() {
                self.ready.push(Completion::new(
                    token,
                    if idx == bad_idx { errno } else { -ECANCELED },
                ));
            }
            return Ok(());
        }
        self.next_chain_id += 1;
        let chain_id = self.next_chain_id;
        for (pos, &(token, op)) in ops.iter().enumerate() {
            self.next_op_id += 1;
            let world_tag = self.world.on_submit(&op);
            self.pending.push(Pending {
                token,
                op,
                deadline: 0,
                cancelled: false,
                id: self.next_op_id,
                cancel_target: None,
                attached: false,
                fresh: true,
                chain_id,
                chain_pos: pos as u32,
                chain_active: pos == 0,
                chain_aborted: false,
                world_tag,
                last_delivered_step: 0,
                unix_path: [0u8; 108],
                unix_len: 0,
            });
        }
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        let mut idle = 0u32;
        loop {
            while !out.is_full() {
                let Some(c) = self.ready.pop() else { break };
                self.record_trace(c);
                out.push(c);
            }
            if !self.ready.is_empty() {
                return Ok(());
            }
            self.step(out);
            if !wait || !out.is_empty() {
                return Ok(());
            }
            let earliest = self
                .pending
                .iter()
                .filter(|p| matches!(p.op, Op::Timeout { .. }))
                .map(|p| p.deadline)
                .min();
            let only_timers = !self.pending.is_empty()
                && self
                    .pending
                    .iter()
                    .all(|p| matches!(p.op, Op::Timeout { .. }));
            self.world.clear_interest();
            for p in &self.pending {
                if !p.cancelled && p.chain_active && !p.chain_aborted {
                    self.world.add_interest(&p.op);
                }
            }
            idle += 1;
            self.world.wait(earliest, only_timers, idle)?;
        }
    }
}
