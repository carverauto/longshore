// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! SQE preparation for [`UringIo`]'s socket operations, one function each.

use super::*;

impl UringIo {
    /// A TCP or UDP connect: a socket first, or a direct connect when the
    /// slot's socket options are set through a file descriptor. `None` when
    /// the operation pushed its own SQE or completed early.
    pub(super) fn prep_connect(
        &mut self,
        i: u32,
        op: Op,
        sock: Sock,
        addr: SocketAddr,
        token: Token,
    ) -> Option<io_uring_sqe> {
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
        Some(io_uring_sqe {
            opcode: IORING_OP_SOCKET,
            fd: i32::from(family),
            off: kind,
            file_index: sock.0 + 1,
            user_data: PHASE_SOCKET,
            ..Default::default()
        })
    }

    pub(super) fn prep_connect_unix(
        &mut self,
        i: u32,
        sock: Sock,
        (path_buf, path_off, path_len): (BufId, u32, u32),
    ) -> io_uring_sqe {
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

    pub(super) fn prep_accept(&mut self, i: u32, listener: Sock, sock: Sock) -> io_uring_sqe {
        assert!(sock.0 < self.limits.sockets_max, "socket slot out of range");
        let s = &mut self.slots[i as usize];
        s.state = State::Accept;
        s.sock = sock.0;
        s.is_stream = true;
        s.op_sock = Some(listener);
        // The kernel writes the peer's address into the slot, which stays
        // put until the accept completes.
        s.addr = sockaddr_any::default();
        s.addr_len = size_of::<sockaddr_any>() as u32;
        let (addr, addr_len) = (&raw mut s.addr as u64, &raw mut s.addr_len as u64);
        self.sock_peer[sock.0 as usize] = None;
        let mut sqe = self.fixed(listener);
        sqe.opcode = IORING_OP_ACCEPT;
        sqe.file_index = sock.0 + 1;
        sqe.addr = addr;
        sqe.off = addr_len;
        sqe
    }

    pub(super) fn prep_armed_accept(&mut self, i: u32, listener: Sock) -> io_uring_sqe {
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

    #[inline]
    pub(super) fn prep_armed_recv(&mut self, i: u32, sock: Sock) -> io_uring_sqe {
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

    #[inline]
    pub(super) fn prep_send(
        &mut self,
        i: u32,
        sock: Sock,
        (buf, off, len): (BufId, u32, u32),
        zero_copy: bool,
    ) -> io_uring_sqe {
        let s = &mut self.slots[i as usize];
        s.op_sock = Some(sock);
        if zero_copy {
            s.state = State::SendZc;
        }
        let mut sqe = self.fixed(sock);
        sqe.addr = self.buf_ptr(buf, off, len);
        sqe.len = len;
        sqe.op_flags = MSG_NOSIGNAL
            | if self.sock_eor[sock.0 as usize] {
                MSG_EOR
            } else {
                0
            };
        if zero_copy {
            sqe.opcode = IORING_OP_SEND_ZC;
            sqe.ioprio = IORING_RECVSEND_FIXED_BUF;
            sqe.buf_index = self.buf_range[buf.0 as usize];
        } else {
            sqe.opcode = IORING_OP_SEND;
        }
        sqe
    }

    /// A receive; `op_flags` is `MSG_DONTWAIT` for one that must not wait.
    #[inline]
    pub(super) fn prep_recv(
        &mut self,
        i: u32,
        sock: Sock,
        (buf, off, len): (BufId, u32, u32),
        op_flags: u32,
    ) -> io_uring_sqe {
        self.slots[i as usize].op_sock = Some(sock);
        let mut s = self.fixed(sock);
        s.opcode = IORING_OP_RECV;
        s.addr = self.buf_ptr(buf, off, len);
        s.len = len;
        s.op_flags = op_flags;
        s
    }

    /// A shutdown of one or both directions; the slot stays open.
    #[inline]
    pub(super) fn prep_shutdown(
        &mut self,
        i: u32,
        sock: Sock,
        how: crate::io::Shut,
    ) -> io_uring_sqe {
        self.slots[i as usize].op_sock = Some(sock);
        let mut s = self.fixed(sock);
        s.opcode = IORING_OP_SHUTDOWN;
        s.len = how.raw();
        s
    }

    /// A close: cancels the slot's other operations, shuts a stream down,
    /// then closes the fixed file.
    pub(super) fn prep_close(&mut self, i: u32, sock: Sock) -> io_uring_sqe {
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
        self.sock_peer[sock.0 as usize] = None;
        self.sock_eor[sock.0 as usize] = false;
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
}
