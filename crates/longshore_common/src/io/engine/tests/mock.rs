// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A world for the engine's tests: what a receive gets is queued per
//! socket, and a receive is ready when its queue is not empty.

use super::super::*;
use crate::io::BufId;
use std::collections::VecDeque;

pub(super) struct MockWorld {
    first_idx: usize,
    pub(super) queues: [VecDeque<Attempt>; 8],
    pub(super) closed_socks: Vec<Sock>,
    pub(super) returned_bufs: Vec<BufId>,
}

impl MockWorld {
    pub(super) fn new(first_idx: usize) -> Self {
        Self {
            first_idx,
            queues: std::array::from_fn(|_| VecDeque::new()),
            closed_socks: Vec::new(),
            returned_bufs: Vec::new(),
        }
    }
}

impl World for MockWorld {
    fn now_ns(&self) -> u64 {
        1_000
    }
    fn unix_secs(&self) -> u64 {
        1_000
    }
    fn register_file(&mut self, _path: &str) -> io::Result<FileId> {
        Ok(FileId(0))
    }
    fn listen(&mut self, _sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        Ok(addr)
    }
    fn listen_unix(&mut self, _sock: Sock, _path: &str) -> io::Result<()> {
        Ok(())
    }
    fn give_back(&mut self, id: BufId) {
        self.returned_bufs.push(id);
    }
    fn is_ready(&self, op: &Op) -> bool {
        match op {
            Op::Recv { sock, .. } | Op::ArmedRecvZc { sock, .. } => {
                !self.queues[sock.0 as usize].is_empty()
            }
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileWriteSpans { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => true,
        }
    }
    fn advance(&mut self) {}
    fn first(&mut self, n: usize) -> usize {
        self.first_idx % n
    }
    fn hold(&mut self, _op: &Op) -> bool {
        false
    }
    fn attempt(&mut self, _op: &Op, _pool: &mut [u8], _limits: &Limits) -> Option<i32> {
        None
    }
    fn attempt_completion(
        &mut self,
        op: &Op,
        _pool: &mut [u8],
        _limits: &Limits,
        _tag: u64,
    ) -> Option<Attempt> {
        match op {
            Op::Recv { sock, .. } | Op::ArmedRecvZc { sock, .. } => {
                self.queues[sock.0 as usize].pop_front()
            }
            Op::Close { .. } => Some(Attempt::single(0)),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileWriteSpans { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => None,
        }
    }
    fn claimed(&self, _op: &Op) -> bool {
        false
    }
    fn release(&mut self, _op: &Op) {}
    fn closed(&mut self, sock: Sock) {
        self.closed_socks.push(sock);
    }
    fn wait(&mut self, _deadline: Option<u64>, _only_timers: bool, _idle: u32) -> io::Result<()> {
        Ok(())
    }
}
