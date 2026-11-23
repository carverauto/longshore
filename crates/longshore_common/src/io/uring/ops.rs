// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! SQE preparation for [`UringIo`] across socket, timer, wake, and file operations.

use super::*;

impl UringIo {
    pub(super) fn prepare_sqe(&mut self, i: u32, op: Op, token: Token) -> Option<io_uring_sqe> {
        let sqe = match op {
            Op::Connect { sock, addr } | Op::ConnectUdp { sock, addr } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                self.sock_stream[sock.0 as usize] = false;
                let s = &mut self.slots[i as usize];
                s.state = State::Connecting;
                s.phase = PHASE_SOCKET;
                s.sock = sock.0;
                s.is_stream = matches!(op, Op::Connect { .. });
                s.op_sock = Some(sock);
                s.probe_fd = -1;
                (s.addr, s.addr_len) = sockaddr_any::from(addr);
                let family = s.addr.family;
                let need_fd_opts = self.no_delay || self.keep_alive;
                if matches!(op, Op::Connect { .. }) && (self.measure_segments || need_fd_opts) {
                    if let Some(sqe) = self.connect_with_fd(i, sock, family, token) {
                        self.push(sqe);
                        return None;
                    }
                    if need_fd_opts {
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
                    fd: i32::from(family),
                    off: kind,
                    file_index: sock.0 + 1,
                    user_data: PHASE_SOCKET,
                    ..Default::default()
                }
            }
            Op::ConnectUnix {
                sock,
                path_buf,
                path_off,
                path_len,
            } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                self.sock_stream[sock.0 as usize] = false;
                let (at, _) = self.limits.span(path_buf);
                let start = self.pool_base + at + path_off as usize;
                let slice = &self.bufs[start..start + path_len as usize];
                let (addr, addr_len) = sockaddr_any::from_unix(slice);
                let s = &mut self.slots[i as usize];
                s.state = State::Connecting;
                s.phase = PHASE_SOCKET;
                s.sock = sock.0;
                s.is_stream = true;
                s.op_sock = Some(sock);
                s.probe_fd = -1;
                s.addr = addr;
                s.addr_len = addr_len;
                io_uring_sqe {
                    opcode: IORING_OP_SOCKET,
                    fd: i32::from(AF_UNIX),
                    off: SOCK_STREAM,
                    file_index: sock.0 + 1,
                    user_data: PHASE_SOCKET,
                    ..Default::default()
                }
            }
            Op::Accept { listener, sock } => {
                assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
                let s = &mut self.slots[i as usize];
                s.state = State::Accept;
                s.sock = sock.0;
                s.is_stream = true;
                s.op_sock = Some(listener);
                let mut sqe = self.fixed(listener);
                sqe.opcode = IORING_OP_ACCEPT;
                sqe.file_index = sock.0 + 1;
                sqe
            }
            Op::ArmedAccept { listener } => {
                let s = &mut self.slots[i as usize];
                s.state = State::ArmedAccept;
                s.is_stream = true;
                s.op_sock = Some(listener);
                let mut sqe = self.fixed(listener);
                sqe.opcode = IORING_OP_ACCEPT;
                sqe.ioprio = IORING_ACCEPT_MULTISHOT;
                sqe.file_index = IORING_FILE_INDEX_ALLOC;
                sqe
            }
            Op::ArmedRecv { sock } => {
                let s = &mut self.slots[i as usize];
                s.state = State::ArmedRecv;
                s.op_sock = Some(sock);
                let mut sqe = self.fixed(sock);
                sqe.opcode = IORING_OP_RECV;
                sqe.flags |= IOSQE_BUFFER_SELECT;
                sqe.ioprio = IORING_RECV_MULTISHOT;
                sqe.buf_index = 0;
                sqe.addr = 0;
                sqe.len = 0;
                sqe
            }
            Op::Send {
                sock,
                buf,
                off,
                len,
                zero_copy,
            } => {
                let s = &mut self.slots[i as usize];
                s.op_sock = Some(sock);
                if zero_copy {
                    s.state = State::SendZc;
                }
                let mut sqe = self.fixed(sock);
                sqe.addr = self.buf_ptr(buf, off, len);
                sqe.len = len;
                sqe.op_flags = MSG_NOSIGNAL;
                if zero_copy {
                    sqe.opcode = IORING_OP_SEND_ZC;
                    sqe.ioprio = IORING_RECVSEND_FIXED_BUF;
                    sqe.buf_index = self.buf_range[buf.0 as usize];
                } else {
                    sqe.opcode = IORING_OP_SEND;
                }
                sqe
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
                if core::mem::replace(&mut self.sock_stream[sock.0 as usize], false) {
                    self.push(io_uring_sqe {
                        opcode: IORING_OP_SHUTDOWN,
                        fd: fixed_fd(sock.0),
                        flags: IOSQE_FIXED_FILE | IOSQE_IO_HARDLINK,
                        len: SHUT_RDWR,
                        user_data: INTERNAL_SHUTDOWN,
                        ..Default::default()
                    });
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
                #[expect(
                    clippy::expect_used,
                    reason = "a FileId comes from register_file; an unknown one is a caller bug, as on the other backends"
                )]
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
                let wait = core::time::Duration::from_nanos(nanos);
                s.ts = timespec {
                    // At most u64::MAX / 10^9 seconds, which always fits.
                    sec: i64::try_from(wait.as_secs()).unwrap_or(i64::MAX),
                    nsec: i64::from(wait.subsec_nanos()),
                };
                io_uring_sqe {
                    opcode: IORING_OP_TIMEOUT,
                    addr: &s.ts as *const timespec as u64,
                    len: 1,
                    ..Default::default()
                }
            }
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
            | Op::FileMkdir { .. } => self.prepare_file_sqe(i, op),
        };
        Some(sqe)
    }
}
