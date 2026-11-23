// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Path, buffer, direct-I/O, and punch validation helpers for [`super`].

use super::{FileSlot, SlotEntry, STATX_SIZE};
use crate::io::{BufId, Limits, Op, EBADF, EINVAL, ENAMETOOLONG};

pub(super) fn check_non_lifecycle(
    slots: &[SlotEntry],
    op: &Op,
    pool: &[u8],
    limits: &Limits,
    use_sim: bool,
) -> Result<(), i32> {
    let slot_ok = |slot: FileSlot| active_slot(slots, slot, use_sim);

    match *op {
        Op::FileRead {
            slot,
            buf,
            off,
            len,
            file_off,
        }
        | Op::FileWrite {
            slot,
            buf,
            off,
            len,
            file_off,
        } => {
            let direct = slot_ok(slot)?;
            check_rw(limits, direct, buf, off, len, file_off)
        }
        Op::FileSync { slot, .. } | Op::FileReserve { slot, .. } => {
            let _: bool = slot_ok(slot)?;
            Ok(())
        }
        Op::FilePunch {
            slot,
            file_off,
            len,
        } => {
            let _: bool = slot_ok(slot)?;
            let bs = u64::from(limits.block_size);
            if bs == 0 || !file_off.is_multiple_of(bs) || !len.is_multiple_of(bs) {
                return Err(-EINVAL);
            }
            Ok(())
        }
        Op::FileStatus { .. }
        | Op::FileRename { .. }
        | Op::FileUnlink { .. }
        | Op::FileMkdir { .. } => check_path_op(op, pool, limits),
        Op::FileOpen { .. } | Op::FileClose { .. } => unreachable!(),
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
        | Op::Cancel { .. } => Err(-EINVAL),
    }
}

/// Checks the paths (and status output range) of a file operation that
/// names paths rather than an open slot.
fn check_path_op(op: &Op, pool: &[u8], limits: &Limits) -> Result<(), i32> {
    match *op {
        Op::FileStatus {
            path_buf,
            path_off,
            path_len,
            out_buf,
            out_off,
        } => {
            check_path(pool, limits, path_buf, path_off, path_len)?;
            check_buf_range(limits, out_buf, out_off, STATX_SIZE as u32)
        }
        Op::FileRename {
            old_buf,
            old_off,
            old_len,
            new_buf,
            new_off,
            new_len,
        } => {
            check_path(pool, limits, old_buf, old_off, old_len)?;
            check_path(pool, limits, new_buf, new_off, new_len)
        }
        Op::FileUnlink {
            path_buf,
            path_off,
            path_len,
        }
        | Op::FileMkdir {
            path_buf,
            path_off,
            path_len,
        } => check_path(pool, limits, path_buf, path_off, path_len),
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
        | Op::Cancel { .. }
        | Op::FileOpen { .. }
        | Op::FileClose { .. }
        | Op::FileRead { .. }
        | Op::FileWrite { .. }
        | Op::FileSync { .. }
        | Op::FileReserve { .. }
        | Op::FilePunch { .. } => Err(-EINVAL),
    }
}

/// Whether `slot` holds an open file (in the chain simulation when
/// `use_sim`); `Ok` carries whether it was opened for direct I/O.
fn active_slot(slots: &[SlotEntry], slot: FileSlot, use_sim: bool) -> Result<bool, i32> {
    let s = slots.get(slot.0 as usize).ok_or(-EBADF)?;
    let (active, direct) = if use_sim {
        (s.sim_active, s.sim_direct)
    } else {
        (s.is_usable(), s.direct)
    };
    if !active {
        return Err(-EBADF);
    }
    Ok(direct)
}

/// Checks a read or write's buffer range, and for a direct-I/O file its
/// buffer, offsets and length against the block alignment.
fn check_rw(
    limits: &Limits,
    direct: bool,
    buf: BufId,
    off: u32,
    len: u32,
    file_off: u64,
) -> Result<(), i32> {
    check_buf_range(limits, buf, off, len)?;
    if !direct {
        return Ok(());
    }
    if !limits.is_aligned_buf(buf) {
        return Err(-EINVAL);
    }
    let bs = u64::from(limits.block_size);
    if bs == 0
        || !u64::from(off).is_multiple_of(bs)
        || !u64::from(len).is_multiple_of(bs)
        || !file_off.is_multiple_of(bs)
    {
        return Err(-EINVAL);
    }
    Ok(())
}

pub(super) fn slot_of(op: &Op) -> Option<FileSlot> {
    match *op {
        Op::FileOpen { slot, .. }
        | Op::FileClose { slot }
        | Op::FileRead { slot, .. }
        | Op::FileWrite { slot, .. }
        | Op::FileSync { slot, .. }
        | Op::FileReserve { slot, .. }
        | Op::FilePunch { slot, .. } => Some(slot),
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
        | Op::Cancel { .. }
        | Op::FileStatus { .. }
        | Op::FileRename { .. }
        | Op::FileUnlink { .. }
        | Op::FileMkdir { .. } => None,
    }
}

pub(super) fn check_buf_range(limits: &Limits, buf: BufId, off: u32, len: u32) -> Result<(), i32> {
    if buf.0 >= limits.buffer_count() {
        return Err(-EINVAL);
    }
    let (_, size) = limits.span(buf);
    if u64::from(off) + u64::from(len) > size as u64 {
        return Err(-EINVAL);
    }
    Ok(())
}

pub(crate) fn check_path(
    pool: &[u8],
    limits: &Limits,
    buf: BufId,
    off: u32,
    len: u32,
) -> Result<(), i32> {
    if len == 0 || buf.0 >= limits.buffer_count() {
        return Err(-EINVAL);
    }
    let (base, size) = limits.span(buf);
    if u64::from(off) + u64::from(len) > size as u64 {
        return Err(-EINVAL);
    }
    let start = base + off as usize;
    let slice = &pool[start..start + len as usize];
    if slice[slice.len() - 1] != 0 || slice.len() == 1 || slice[..slice.len() - 1].contains(&0) {
        return Err(-EINVAL);
    }
    Ok(())
}

pub(crate) fn check_unix_path<'a>(
    pool: &'a [u8],
    limits: &Limits,
    buf: BufId,
    off: u32,
    len: u32,
    max_sun_path: usize,
) -> Result<&'a [u8], i32> {
    check_path(pool, limits, buf, off, len)?;
    if len as usize > max_sun_path {
        return Err(-ENAMETOOLONG);
    }
    let (base, _) = limits.span(buf);
    let start = base + off as usize;
    Ok(&pool[start..start + len as usize])
}
