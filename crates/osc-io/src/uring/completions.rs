// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Turning kernel completions into results, including operations of several steps.

use super::*;

impl UringIo {
    pub(super) fn connect_sqe(&self, i: u32) -> io_uring_sqe {
        let s = &self.slots[i as usize];
        let mut sqe = self.fixed(Sock(s.sock));
        sqe.opcode = IORING_OP_CONNECT;
        sqe.addr = &s.addr as *const sockaddr_in as u64;
        sqe.off = core::mem::size_of::<sockaddr_in>() as u64;
        sqe.user_data = i as u64;
        sqe
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
        if state == State::ConnectStep && c.res < 0 {
            return self.release_socket(i, c.res);
        }
        // Only a timer's expiry is a success. The same code from any other
        // operation is a failure and is reported as one.
        let r = if state == State::Timer && c.res == -ETIME {
            0
        } else {
            c.res
        };
        self.finish(i, r, out)
    }
}
