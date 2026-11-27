// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a submitted operation is, as far as the checks of the set care:
//! which file operation, of which slot, at which offset.

use longshore_common::io::{FileSlot, Op};

/// The file operations, and everything else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `Op::FileOpen`.
    Open,
    /// `Op::FileClose`.
    Close,
    /// `Op::FileRead`.
    Read,
    /// `Op::FileWrite`.
    Write,
    /// `Op::FileWriteSpans`.
    WriteSpans,
    /// `Op::FileSync`.
    Sync,
    /// `Op::FileReserve`.
    Reserve,
    /// `Op::FilePunch`.
    Punch,
    /// `Op::FileStatus`.
    Status,
    /// `Op::FileRename`.
    Rename,
    /// `Op::FileUnlink`.
    Unlink,
    /// `Op::FileMkdir`.
    Mkdir,
    /// Not a file operation.
    Other,
}

/// What `op` is, its file slot, and its file offset where it has them.
pub fn describe(op: &Op) -> (Kind, Option<FileSlot>, Option<u64>) {
    match *op {
        Op::FileOpen { slot, .. } => (Kind::Open, Some(slot), None),
        Op::FileClose { slot } => (Kind::Close, Some(slot), None),
        Op::FileRead { slot, file_off, .. } => (Kind::Read, Some(slot), Some(file_off)),
        Op::FileWrite { slot, file_off, .. } => (Kind::Write, Some(slot), Some(file_off)),
        Op::FileWriteSpans { slot, file_off, .. } => (Kind::WriteSpans, Some(slot), Some(file_off)),
        Op::FileSync { slot, .. } => (Kind::Sync, Some(slot), None),
        Op::FileReserve { slot, file_off, .. } => (Kind::Reserve, Some(slot), Some(file_off)),
        Op::FilePunch { slot, file_off, .. } => (Kind::Punch, Some(slot), Some(file_off)),
        Op::FileStatus { .. } => (Kind::Status, None, None),
        Op::FileRename { .. } => (Kind::Rename, None, None),
        Op::FileUnlink { .. } => (Kind::Unlink, None, None),
        Op::FileMkdir { .. } => (Kind::Mkdir, None, None),
        Op::Connect { .. }
        | Op::ConnectUnix { .. }
        | Op::ConnectUdp { .. }
        | Op::Accept { .. }
        | Op::ArmedAccept { .. }
        | Op::ArmedRecv { .. }
        | Op::ArmedRecvZc { .. }
        | Op::Send { .. }
        | Op::SendZc { .. }
        | Op::Recv { .. }
        | Op::RecvNow { .. }
        | Op::Shutdown { .. }
        | Op::Close { .. }
        | Op::AwaitWake
        | Op::ReadFile { .. }
        | Op::Timeout { .. }
        | Op::Cancel { .. } => (Kind::Other, None, None),
    }
}
