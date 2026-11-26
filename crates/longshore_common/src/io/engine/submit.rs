// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The per-operation facts `Engine::submit` records with a pending operation.

use super::*;

impl<W: World> Engine<W> {
    /// Checks a socket operation's preconditions before it is queued. Returns
    /// the Unix-domain path it carries (empty for every other operation), or
    /// the errno it completes with at once.
    pub(super) fn socket_precheck(&self, op: &Op) -> Result<([u8; 108], u8), i32> {
        match *op {
            Op::ArmedAccept { listener } => {
                assert!(
                    listener.0 < self.limits.sockets_max,
                    "socket slot out of range"
                );
                if self.limits.accept_range_len == 0 {
                    return Err(-ENFILE);
                }
                Ok(([0u8; 108], 0))
            }
            Op::ArmedRecv { sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                if self.limits.ring_buffers == 0 {
                    return Err(-ENOBUFS);
                }
                Ok(([0u8; 108], 0))
            }
            Op::ConnectUnix {
                sock,
                path_buf,
                path_off,
                path_len,
            } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let pool = &self.bufs[self.pool_base..];
                let slice = crate::io::files::check_unix_path(
                    pool,
                    &self.limits,
                    path_buf,
                    path_off,
                    path_len,
                    108,
                )?;
                let mut buf = [0u8; 108];
                buf[..slice.len()].copy_from_slice(slice);
                Ok((buf, slice.len() as u8))
            }
            Op::Connect { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::Recv { .. }
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
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => Ok(([0u8; 108], 0)),
        }
    }

    /// The virtual time a timeout fires at; 0 for every other operation.
    pub(super) fn deadline_of(&self, op: &Op) -> u64 {
        match *op {
            Op::Timeout { nanos } => self.world.now_ns().saturating_add(nanos),
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
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
        }
    }

    /// The id of the live, cancellable operation a cancel targets, if any;
    /// `None` for every other operation.
    pub(super) fn cancel_target_of(&self, op: &Op) -> Option<u64> {
        match *op {
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
            | Op::SendZc { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
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
        }
    }
}
