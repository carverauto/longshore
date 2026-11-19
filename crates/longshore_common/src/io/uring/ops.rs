// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! SQE preparation for [`UringIo`] across socket, timer, wake, and file operations.

use super::*;
use crate::io::files::STATX_SIZE;

impl UringIo {
    pub(super) fn prepare_sqe(&mut self, i: u32, op: Op) -> Option<io_uring_sqe> {
        let sqe = match op {
            Op::Connect { sock, addr } | Op::ConnectUdp { sock, addr } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let s = &mut self.slots[i as usize];
                s.state = State::Connecting;
                s.phase = PHASE_SOCKET;
                s.sock = sock.0;
                s.op_sock = Some(sock);
                s.probe_fd = -1;
                (s.addr, s.addr_len) = sockaddr_any::from(addr);
                let family = s.addr.family;
                if self.measure_segments && matches!(op, Op::Connect { .. }) {
                    if let Some(sqe) = self.connect_measured(i, sock, family) {
                        self.push(sqe);
                        return None;
                    }
                }
                let kind = if matches!(op, Op::ConnectUdp { .. }) {
                    SOCK_DGRAM
                } else {
                    SOCK_STREAM
                };
                io_uring_sqe {
                    opcode: IORING_OP_SOCKET,
                    fd: family as i32,
                    off: kind,
                    file_index: sock.0 + 1,
                    user_data: PHASE_SOCKET,
                    ..Default::default()
                }
            }
            Op::Accept { listener, sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                self.slots[i as usize].op_sock = Some(listener);
                let mut s = self.fixed(listener);
                s.opcode = IORING_OP_ACCEPT;
                s.file_index = sock.0 + 1;
                s
            }
            Op::Send {
                sock,
                buf,
                off,
                len,
                zero_copy,
            } => {
                self.slots[i as usize].op_sock = Some(sock);
                let mut s = self.fixed(sock);
                s.addr = self.buf_ptr(buf, off, len);
                s.len = len;
                s.op_flags = MSG_NOSIGNAL;
                if zero_copy {
                    s.opcode = IORING_OP_SEND_ZC;
                    s.ioprio = IORING_RECVSEND_FIXED_BUF;
                    s.buf_index = self.buf_range[buf.0 as usize];
                } else {
                    s.opcode = IORING_OP_SEND;
                }
                s
            }
            Op::Recv {
                sock,
                buf,
                off,
                len,
            } => {
                self.slots[i as usize].op_sock = Some(sock);
                let mut s = self.fixed(sock);
                s.opcode = IORING_OP_RECV;
                s.addr = self.buf_ptr(buf, off, len);
                s.len = len;
                s
            }
            Op::RecvNow {
                sock,
                buf,
                off,
                len,
            } => {
                self.slots[i as usize].op_sock = Some(sock);
                let mut s = self.fixed(sock);
                s.opcode = IORING_OP_RECV;
                s.addr = self.buf_ptr(buf, off, len);
                s.len = len;
                s.op_flags = MSG_DONTWAIT;
                s
            }
            Op::Close { sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                self.slots[i as usize].state = State::Close;
                for k in 0..self.slots.len() {
                    if k as u32 == i || self.slots[k].op_sock != Some(sock) {
                        continue;
                    }
                    if matches!(
                        self.slots[k].state,
                        State::Free
                            | State::Close
                            | State::FileClose
                            | State::FileSlotClose
                            | State::ConnectRelease
                            | State::AwaitNotif
                    ) {
                        continue;
                    }
                    self.slots[k].sock_closed = true;
                    if !self.slots[k].cancel_requested {
                        self.slots[k].cancel_requested = true;
                        let addr = k as u64 | self.slots[k].phase;
                        self.push(io_uring_sqe {
                            opcode: IORING_OP_ASYNC_CANCEL,
                            fd: -1,
                            addr,
                            user_data: INTERNAL_CANCEL,
                            ..Default::default()
                        });
                    }
                }
                io_uring_sqe {
                    opcode: IORING_OP_CLOSE,
                    file_index: sock.0 + 1,
                    ..Default::default()
                }
            }
            Op::Cancel { target } => {
                let found = self
                    .slots
                    .iter()
                    .position(|s| s.state != State::Free && s.token == target.0)
                    .filter(|k| *k as u32 != i);
                let ud = match found {
                    Some(k)
                        if matches!(
                            self.slots[k].state,
                            State::Close
                                | State::FileClose
                                | State::FileSlotClose
                                | State::ConnectRelease
                        ) =>
                    {
                        u64::MAX
                    }
                    Some(k) => {
                        self.slots[k].cancel_requested = true;
                        k as u64 | self.slots[k].phase
                    }
                    None => u64::MAX,
                };
                io_uring_sqe {
                    opcode: IORING_OP_ASYNC_CANCEL,
                    fd: -1,
                    addr: ud,
                    ..Default::default()
                }
            }
            Op::ReadFile {
                file,
                buf,
                off,
                len,
            } => {
                let addr = self.buf_ptr(buf, off, len);
                let path = self
                    .files
                    .get(file.0 as usize)
                    .expect("file was registered")
                    .as_ptr() as u64;
                let s = &mut self.slots[i as usize];
                (s.state, s.phase, s.file_addr, s.file_len) =
                    (State::FileOpen, PHASE_FILE_OPEN, addr, len);
                io_uring_sqe {
                    opcode: IORING_OP_OPENAT,
                    fd: AT_FDCWD,
                    addr: path,
                    op_flags: O_RDONLY_CLOEXEC,
                    user_data: PHASE_FILE_OPEN,
                    ..Default::default()
                }
            }
            Op::AwaitWake => {
                assert!(self.wake_fd >= 0, "Op::AwaitWake before Io::waker");
                let s = &mut self.slots[i as usize];
                s.state = State::Wake;
                io_uring_sqe {
                    opcode: IORING_OP_READ,
                    fd: self.wake_fd,
                    addr: &s.wake_count as *const u64 as u64,
                    len: 8,
                    ..Default::default()
                }
            }
            Op::Timeout { nanos } => {
                let s = &mut self.slots[i as usize];
                s.state = State::Timer;
                s.ts = timespec {
                    sec: (nanos / 1_000_000_000) as i64,
                    nsec: (nanos % 1_000_000_000) as i64,
                };
                io_uring_sqe {
                    opcode: IORING_OP_TIMEOUT,
                    addr: &s.ts as *const timespec as u64,
                    len: 1,
                    ..Default::default()
                }
            }
            _ => self.prepare_file_sqe(i, op),
        };
        Some(sqe)
    }

    fn prepare_file_sqe(&mut self, i: u32, op: Op) -> io_uring_sqe {
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
                    len: AT_FDCWD as u32,
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
            _ => unreachable!(),
        }
    }
}
