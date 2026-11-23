// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! File-slot and path SQE preparation for [`UringIo`].

use super::*;
use crate::io::files::STATX_SIZE;

impl UringIo {
    pub(super) fn prepare_file_sqe(&mut self, i: u32, op: Op) -> io_uring_sqe {
        match op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => {
                let s = &mut self.slots[i as usize];
                s.state = State::FileSlotOpen;
                s.file_slot = slot;
                s.file_direct = flags.direct;
                io_uring_sqe {
                    opcode: IORING_OP_OPENAT,
                    fd: AT_FDCWD,
                    addr: self.buf_ptr(path_buf, path_off, path_len),
                    len: if flags.create { 0o666 } else { 0 },
                    op_flags: open_flags_linux(flags),
                    file_index: self.limits.sockets_max + slot.0 + 1,
                    ..Default::default()
                }
            }
            Op::FileClose { slot } => {
                let s = &mut self.slots[i as usize];
                s.state = State::FileSlotClose;
                s.file_slot = slot;
                io_uring_sqe {
                    opcode: IORING_OP_CLOSE,
                    file_index: self.limits.sockets_max + slot.0 + 1,
                    ..Default::default()
                }
            }
            Op::FileRead {
                slot,
                buf,
                off,
                len,
                file_off,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                let mut sqe = self.fixed_file(slot);
                sqe.opcode = IORING_OP_READ_FIXED;
                sqe.off = file_off;
                sqe.addr = self.buf_ptr(buf, off, len);
                sqe.len = len;
                sqe.buf_index = self.buf_range[buf.0 as usize];
                sqe
            }
            Op::FileWrite {
                slot,
                buf,
                off,
                len,
                file_off,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                let mut sqe = self.fixed_file(slot);
                sqe.opcode = IORING_OP_WRITE_FIXED;
                sqe.off = file_off;
                sqe.addr = self.buf_ptr(buf, off, len);
                sqe.len = len;
                sqe.buf_index = self.buf_range[buf.0 as usize];
                sqe
            }
            Op::FileSync { slot, datasync } => {
                self.slots[i as usize].state = State::FileSlotOp;
                let mut sqe = self.fixed_file(slot);
                sqe.opcode = IORING_OP_FSYNC;
                sqe.op_flags = if datasync { IORING_FSYNC_DATASYNC } else { 0 };
                sqe
            }
            Op::FileReserve {
                slot,
                file_off,
                len,
                keep_size,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                let mut sqe = self.fixed_file(slot);
                sqe.opcode = IORING_OP_FALLOCATE;
                sqe.off = file_off;
                sqe.addr = len;
                sqe.len = if keep_size { FALLOC_FL_KEEP_SIZE } else { 0 };
                sqe
            }
            Op::FilePunch {
                slot,
                file_off,
                len,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                let mut sqe = self.fixed_file(slot);
                sqe.opcode = IORING_OP_FALLOCATE;
                sqe.off = file_off;
                sqe.addr = len;
                sqe.len = FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE;
                sqe
            }
            Op::FileStatus {
                path_buf,
                path_off,
                path_len,
                out_buf,
                out_off,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                io_uring_sqe {
                    opcode: IORING_OP_STATX,
                    fd: AT_FDCWD,
                    addr: self.buf_ptr(path_buf, path_off, path_len),
                    len: STATX_BASIC_STATS,
                    off: self.buf_ptr(out_buf, out_off, STATX_SIZE as u32),
                    ..Default::default()
                }
            }
            Op::FileRename {
                old_buf,
                old_off,
                old_len,
                new_buf,
                new_off,
                new_len,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                io_uring_sqe {
                    opcode: IORING_OP_RENAMEAT,
                    fd: AT_FDCWD,
                    addr: self.buf_ptr(old_buf, old_off, old_len),
                    len: AT_FDCWD_SQE_LEN,
                    off: self.buf_ptr(new_buf, new_off, new_len),
                    ..Default::default()
                }
            }
            Op::FileUnlink {
                path_buf,
                path_off,
                path_len,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                io_uring_sqe {
                    opcode: IORING_OP_UNLINKAT,
                    fd: AT_FDCWD,
                    addr: self.buf_ptr(path_buf, path_off, path_len),
                    ..Default::default()
                }
            }
            Op::FileMkdir {
                path_buf,
                path_off,
                path_len,
            } => {
                self.slots[i as usize].state = State::FileSlotOp;
                io_uring_sqe {
                    opcode: IORING_OP_MKDIRAT,
                    fd: AT_FDCWD,
                    addr: self.buf_ptr(path_buf, path_off, path_len),
                    len: 0o777,
                    ..Default::default()
                }
            }
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
            | Op::Cancel { .. } => unreachable!(),
        }
    }
}
