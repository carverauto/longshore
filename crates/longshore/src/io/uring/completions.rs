// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Turning kernel completions into results, including operations of several steps.

use super::*;
use crate::io::Segment;

impl UringIo {
    pub(super) fn connect_sqe(&self, i: u32) -> io_uring_sqe {
        let s = &self.slots[i as usize];
        let mut sqe = self.fixed(Sock(s.sock));
        sqe.opcode = IORING_OP_CONNECT;
        sqe.addr = &s.addr as *const sockaddr_any as u64;
        sqe.off = s.addr_len as u64;
        sqe.user_data = i as u64;
        sqe
    }

    /// Open the socket of a connect with a descriptor of our own, so its
    /// segment size can be read later, and register it in the slot. Returns
    /// the connect to submit, or None if the socket could not be opened or
    /// registered: the caller then creates it through the ring.
    pub(super) fn connect_measured(
        &mut self,
        i: u32,
        sock: Sock,
        family: u16,
    ) -> Option<io_uring_sqe> {
        let kind = (SOCK_STREAM | SOCK_CLOEXEC) as i32;
        let fd = unsafe { socket(family as i32, kind, 0) };
        if fd < 0 {
            return None;
        }
        if self.ring.update_file(sock.0, fd).is_err() {
            unsafe { close(fd) };
            return None;
        }
        let s = &mut self.slots[i as usize];
        s.probe_fd = fd;
        s.state = State::ConnectStep;
        s.phase = 0;
        Some(self.connect_sqe(i))
    }

    /// The connect of a measured socket has ended. Read the segment sizes if
    /// it succeeded, and close our descriptor either way: the registered
    /// table keeps the socket. Returns the connect's result, with the
    /// encoded sizes in place of 0.
    fn end_measure(&mut self, i: u32, result: i32) -> i32 {
        let s = &mut self.slots[i as usize];
        let fd = core::mem::replace(&mut s.probe_fd, -1);
        if fd < 0 {
            return result;
        }
        let option = |level, name| {
            let mut value: i32 = 0;
            let mut len = core::mem::size_of::<i32>() as u32;
            let r =
                unsafe { getsockopt(fd, level, name, (&mut value as *mut i32).cast(), &mut len) };
            if r == 0 && value > 0 {
                value as u32
            } else {
                0
            }
        };
        let mut measured = result;
        if result == 0 {
            let size = option(IPPROTO_TCP, TCP_MAXSEG);
            // The path's MTU less the IP and TCP headers.
            let path_limit = if s.addr.family == AF_INET6 {
                option(IPPROTO_IPV6, IPV6_MTU).saturating_sub(60)
            } else {
                option(IPPROTO_IP, IP_MTU).saturating_sub(40)
            };
            if size > 0 {
                measured = Segment { size, path_limit }.encode();
            }
        }
        unsafe { close(fd) };
        measured
    }

    /// A connect failed after its socket was created. Close the socket, then
    /// report `result`: no caller has to remember to release the slot.
    pub(super) fn release_socket(&mut self, i: u32, result: i32) {
        let s = &mut self.slots[i as usize];
        s.result = result;
        s.state = State::ConnectRelease;
        s.phase = PHASE_SOCK_RELEASE;
        let sqe = io_uring_sqe {
            opcode: IORING_OP_CLOSE,
            file_index: s.sock + 1,
            user_data: PHASE_SOCK_RELEASE | i as u64,
            ..Default::default()
        };
        self.push(sqe)
    }

    pub(super) fn finish(&mut self, i: u32, result: i32, out: &mut Completions) {
        let s = &mut self.slots[i as usize];
        out.push(Completion {
            token: Token(s.token),
            result,
        });
        s.state = State::Free;
        self.free.push(i);
    }

    pub(super) fn on_cqe(&mut self, c: io_uring_cqe, out: &mut Completions) {
        let i = c.user_data as u32;
        let state = self.slots[i as usize].state;
        if c.flags & IORING_CQE_F_NOTIF != 0 {
            match state {
                State::AwaitNotif => {
                    let r = self.slots[i as usize].result;
                    self.finish(i, r, out)
                }
                _ => self.slots[i as usize].state = State::AwaitResult,
            }
            return;
        }
        match c.user_data & (7 << 32) {
            PHASE_FILE_OPEN => {
                if c.res < 0 {
                    return self.finish(i, c.res, out);
                }
                let s = &mut self.slots[i as usize];
                s.fd = c.res;
                if s.cancel_requested {
                    // Cancelled while the open was finishing: do not read, just close.
                    s.result = -ECANCELED;
                    s.state = State::FileClose;
                    s.phase = PHASE_FILE_CLOSE;
                    let sqe = io_uring_sqe {
                        opcode: IORING_OP_CLOSE,
                        fd: s.fd,
                        user_data: PHASE_FILE_CLOSE | i as u64,
                        ..Default::default()
                    };
                    return self.push(sqe);
                }
                s.state = State::FileRead;
                s.phase = PHASE_FILE_READ;
                let sqe = io_uring_sqe {
                    opcode: IORING_OP_READ,
                    fd: s.fd,
                    addr: s.file_addr,
                    len: s.file_len,
                    user_data: PHASE_FILE_READ | i as u64,
                    ..Default::default()
                };
                return self.push(sqe);
            }
            PHASE_FILE_READ => {
                // Close whatever the read returned, then report the read.
                let s = &mut self.slots[i as usize];
                s.result = c.res;
                s.state = State::FileClose;
                s.phase = PHASE_FILE_CLOSE;
                let sqe = io_uring_sqe {
                    opcode: IORING_OP_CLOSE,
                    fd: s.fd,
                    user_data: PHASE_FILE_CLOSE | i as u64,
                    ..Default::default()
                };
                return self.push(sqe);
            }
            PHASE_FILE_CLOSE | PHASE_SOCK_RELEASE => {
                let r = self.slots[i as usize].result;
                return self.finish(i, r, out);
            }
            _ => {}
        }
        if c.user_data & (7 << 32) == PHASE_SOCKET {
            if c.res < 0 {
                return self.finish(i, c.res, out);
            }
            if self.slots[i as usize].cancel_requested {
                // Cancelled while the socket was being created: do not start
                // the connect, which nothing would cancel.
                return self.release_socket(i, -ECANCELED);
            }
            self.slots[i as usize].state = State::ConnectStep;
            self.slots[i as usize].phase = 0;
            let sqe = self.connect_sqe(i);
            return self.push(sqe);
        }
        if c.flags & IORING_CQE_F_MORE != 0 {
            // First half of a zero-copy send; the buffer is still pinned.
            let s = &mut self.slots[i as usize];
            if s.state == State::AwaitResult {
                return self.finish(i, c.res, out);
            }
            s.result = c.res;
            s.state = State::AwaitNotif;
            return;
        }
        if state == State::ConnectStep {
            let r = self.end_measure(i, c.res);
            if r < 0 {
                return self.release_socket(i, r);
            }
            return self.finish(i, r, out);
        }
        // Only a timer's expiry is a success. The same code from any other
        // operation is a failure and is reported as one.
        let r =
            if (state == State::Timer && c.res == -ETIME) || (state == State::Wake && c.res == 8) {
                0
            } else {
                c.res
            };
        self.finish(i, r, out)
    }
}
