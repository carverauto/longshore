// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! File-slot and path SQE preparation for [`UringIo`].

use super::*;
use crate::io::files::STATX_SIZE;
use crate::io::OpenFlags;

impl UringIo {
    pub(super) fn prepare_file_sqe(&mut self, i: u32, op: Op) -> io_uring_sqe {
        match op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => self.prep_file_open(i, slot, (path_buf, path_off, path_len), flags),
            Op::FileClose { slot } => self.prep_file_close(i, slot),
            Op::FileRead {
                slot,
                buf,
                off,
                len,
                file_off,
            } => self.prep_file_rw(i, slot, IORING_OP_READ_FIXED, (buf, off, len), file_off),
            Op::FileWrite {
                slot,
                buf,
                off,
                len,
                file_off,
            } => self.prep_file_rw(i, slot, IORING_OP_WRITE_FIXED, (buf, off, len), file_off),
            // Refused when it was checked: the ring does not gather yet.
            Op::FileWriteSpans { .. } => unreachable!("refused at submit"),
            Op::FileSync { slot, datasync } => {
                let mut sqe = self.slot_sqe(i, slot, IORING_OP_FSYNC);
                sqe.op_flags = if datasync { IORING_FSYNC_DATASYNC } else { 0 };
                sqe
            }
            Op::FileReserve {
                slot,
                file_off,
                len,
                keep_size,
            } => {
                let mode = if keep_size { FALLOC_FL_KEEP_SIZE } else { 0 };
                self.prep_fallocate(i, slot, file_off, len, mode)
            }
            Op::FilePunch {
                slot,
                file_off,
                len,
            } => {
                let mode = FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE;
                self.prep_fallocate(i, slot, file_off, len, mode)
            }
            Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => self.prep_path_op(i, op),
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
            | Op::Cancel { .. } => unreachable!(),
        }
    }

    /// Opens a path into the fixed-file slot that follows the sockets.
    fn prep_file_open(
        &mut self,
        i: u32,
        slot: FileSlot,
        (path_buf, path_off, path_len): (BufId, u32, u32),
        flags: OpenFlags,
    ) -> io_uring_sqe {
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

    fn prep_file_close(&mut self, i: u32, slot: FileSlot) -> io_uring_sqe {
        let s = &mut self.slots[i as usize];
        s.state = State::FileSlotClose;
        s.file_slot = slot;
        io_uring_sqe {
            opcode: IORING_OP_CLOSE,
            file_index: self.limits.sockets_max + slot.0 + 1,
            ..Default::default()
        }
    }

    /// An operation `opcode` on the open file in `slot`.
    #[inline]
    fn slot_sqe(&mut self, i: u32, slot: FileSlot, opcode: u8) -> io_uring_sqe {
        self.slots[i as usize].state = State::FileSlotOp;
        let mut sqe = self.fixed_file(slot);
        sqe.opcode = opcode;
        sqe
    }

    /// A fixed-buffer read or write (`opcode`) at `file_off`.
    #[inline]
    fn prep_file_rw(
        &mut self,
        i: u32,
        slot: FileSlot,
        opcode: u8,
        (buf, off, len): (BufId, u32, u32),
        file_off: u64,
    ) -> io_uring_sqe {
        let mut sqe = self.slot_sqe(i, slot, opcode);
        sqe.off = file_off;
        sqe.addr = self.buf_ptr(buf, off, len);
        sqe.len = len;
        sqe.buf_index = self.buf_range[buf.0 as usize];
        sqe
    }

    /// An `fallocate` of `len` bytes at `file_off` in `mode`.
    fn prep_fallocate(
        &mut self,
        i: u32,
        slot: FileSlot,
        file_off: u64,
        len: u64,
        mode: u32,
    ) -> io_uring_sqe {
        let mut sqe = self.slot_sqe(i, slot, IORING_OP_FALLOCATE);
        sqe.off = file_off;
        sqe.addr = len;
        sqe.len = mode;
        sqe
    }

    /// A file operation that names paths rather than an open slot.
    fn prep_path_op(&mut self, i: u32, op: Op) -> io_uring_sqe {
        match op {
            Op::FileStatus {
                path_buf,
                path_off,
                path_len,
                out_buf,
                out_off,
            } => {
                let out = self.buf_ptr(out_buf, out_off, STATX_SIZE as u32);
                let path = (path_buf, path_off, path_len);
                self.path_sqe(i, IORING_OP_STATX, path, STATX_BASIC_STATS, out)
            }
            Op::FileRename {
                old_buf,
                old_off,
                old_len,
                new_buf,
                new_off,
                new_len,
            } => {
                let new = self.buf_ptr(new_buf, new_off, new_len);
                let old = (old_buf, old_off, old_len);
                self.path_sqe(i, IORING_OP_RENAMEAT, old, AT_FDCWD_SQE_LEN, new)
            }
            Op::FileUnlink {
                path_buf,
                path_off,
                path_len,
            } => self.path_sqe(i, IORING_OP_UNLINKAT, (path_buf, path_off, path_len), 0, 0),
            Op::FileMkdir {
                path_buf,
                path_off,
                path_len,
            } => self.path_sqe(
                i,
                IORING_OP_MKDIRAT,
                (path_buf, path_off, path_len),
                0o777,
                0,
            ),
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
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileWriteSpans { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. } => unreachable!(),
        }
    }

    /// An `*at` operation `opcode` relative to the working directory on the
    /// path in `path`, with the opcode's `len` and `off` fields.
    fn path_sqe(
        &mut self,
        i: u32,
        opcode: u8,
        (buf, off, len): (BufId, u32, u32),
        sqe_len: u32,
        sqe_off: u64,
    ) -> io_uring_sqe {
        self.slots[i as usize].state = State::FileSlotOp;
        io_uring_sqe {
            opcode,
            fd: AT_FDCWD,
            addr: self.buf_ptr(buf, off, len),
            len: sqe_len,
            off: sqe_off,
            ..Default::default()
        }
    }
}
