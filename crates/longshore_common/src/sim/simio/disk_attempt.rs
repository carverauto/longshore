// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Dispatch of file operations to [`SimDisk`], with placed faults applied.

use super::{faults, DiskFaultCtx, PlacedFault, SimDisk};
use crate::io::files::STATX_SIZE;
use crate::io::posix::errno::linux::{EBADF, EINVAL, EIO};
use crate::io::{FileSlot, Limits, Op, OpenFlags};

impl SimDisk {
    pub(in crate::sim::simio) fn attempt(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        op_num: u64,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        let placed = faults::take_placed(ctx.placed, op_num);
        match *op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                self.attempt_open(slot, &pool[r], flags, placed, ctx)
            }
            Op::FileClose { slot } => self.attempt_close(slot, placed, ctx),
            Op::FileRead {
                slot,
                buf,
                off,
                len,
                file_off,
            } => {
                let r = limits.range(buf, off, len);
                self.read(slot, pool, r, file_off, placed, ctx)
            }
            Op::FileWrite {
                slot,
                buf,
                off,
                len,
                file_off,
            } => {
                let r = limits.range(buf, off, len);
                self.write(slot, pool, r, file_off, placed, ctx)
            }
            Op::FileSync { slot, .. } => self.attempt_sync(slot, placed, ctx),
            Op::FileReserve {
                slot,
                file_off,
                len,
                keep_size,
            } => self.reserve(slot, file_off, len, keep_size, placed, ctx),
            Op::FilePunch {
                slot,
                file_off,
                len,
            } => self.punch(slot, file_off, len, placed, ctx),
            Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => self.attempt_path_op(op, pool, limits, placed, ctx),
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
            | Op::Cancel { .. } => -EINVAL,
        }
    }

    fn attempt_open(
        &mut self,
        slot: FileSlot,
        path: &[u8],
        flags: OpenFlags,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        match placed {
            Some(PlacedFault::FailAfter) => {
                let res = self.open(slot, path, flags);
                if res == 0 {
                    self.undo_open_handle(slot);
                }
                ctx.counters.placed_fail_after += 1;
                -EIO
            }
            Some(_) => {
                ctx.counters.placed_fail += 1;
                -EIO
            }
            None => self.open(slot, path, flags),
        }
    }

    /// Runs a file operation that names paths rather than an open slot.
    fn attempt_path_op(
        &mut self,
        op: &Op,
        pool: &mut [u8],
        limits: &Limits,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        match *op {
            Op::FileStatus {
                path_buf,
                path_off,
                path_len,
                out_buf,
                out_off,
            } => {
                let pr = limits.range(path_buf, path_off, path_len);
                let or = limits.range(out_buf, out_off, STATX_SIZE as u32);
                self.attempt_meta(placed, ctx, |d| d.status(pool, pr, or))
            }
            Op::FileRename {
                old_buf,
                old_off,
                old_len,
                new_buf,
                new_off,
                new_len,
            } => {
                let r1 = limits.range(old_buf, old_off, old_len);
                let r2 = limits.range(new_buf, new_off, new_len);
                self.attempt_meta(placed, ctx, |d| d.rename(pool, r1, r2))
            }
            Op::FileUnlink {
                path_buf,
                path_off,
                path_len,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                self.attempt_meta(placed, ctx, |d| d.unlink(&pool[r]))
            }
            Op::FileMkdir {
                path_buf,
                path_off,
                path_len,
            } => {
                let r = limits.range(path_buf, path_off, path_len);
                self.attempt_meta(placed, ctx, |d| d.mkdir(&pool[r]))
            }
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
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. } => -EINVAL,
        }
    }

    fn attempt_close(
        &mut self,
        slot: FileSlot,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
    ) -> i32 {
        if self
            .open_slots
            .get(slot.0 as usize)
            .and_then(|s| *s)
            .is_none()
        {
            return -EBADF;
        }
        match placed {
            Some(PlacedFault::FailAfter) => {
                let _: i32 = self.close(slot);
                ctx.counters.placed_fail_after += 1;
                -EIO
            }
            Some(_) => {
                let _: i32 = self.close(slot);
                ctx.counters.placed_fail += 1;
                -EIO
            }
            None => self.close(slot),
        }
    }

    fn attempt_meta(
        &mut self,
        placed: Option<PlacedFault>,
        ctx: &mut DiskFaultCtx<'_>,
        f: impl FnOnce(&mut Self) -> i32,
    ) -> i32 {
        match placed {
            Some(PlacedFault::FailAfter) => {
                let _: i32 = f(self);
                ctx.counters.placed_fail_after += 1;
                -EIO
            }
            Some(_) => {
                ctx.counters.placed_fail += 1;
                -EIO
            }
            None => f(self),
        }
    }
}
