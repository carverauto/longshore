// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Completions that start the next step of a multi-step operation (a legacy
//! file read, a connect's socket) for [`UringIo`].

use super::*;

impl UringIo {
    /// A legacy file's open finished: read it, or close it if cancelled.
    pub(super) fn on_file_open_cqe(&mut self, i: u32, res: i32, out: &mut Completions) {
        if res < 0 {
            return self.finish(i, res, out);
        }
        let s = &mut self.slots[i as usize];
        s.fd = res;
        if s.cancel_requested {
            // Cancelled while the open was finishing: do not read, just close.
            s.result = -ECANCELED;
            s.state = State::FileClose;
            s.phase = PHASE_FILE_CLOSE;
            let sqe = io_uring_sqe {
                opcode: IORING_OP_CLOSE,
                fd: s.fd,
                user_data: PHASE_FILE_CLOSE | u64::from(i),
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
            user_data: PHASE_FILE_READ | u64::from(i),
            ..Default::default()
        };
        self.push(sqe)
    }

    /// A legacy file's read finished: close whatever the read returned,
    /// then report the read.
    pub(super) fn on_file_read_cqe(&mut self, i: u32, res: i32) {
        let s = &mut self.slots[i as usize];
        s.result = res;
        s.state = State::FileClose;
        s.phase = PHASE_FILE_CLOSE;
        let sqe = io_uring_sqe {
            opcode: IORING_OP_CLOSE,
            fd: s.fd,
            user_data: PHASE_FILE_CLOSE | u64::from(i),
            ..Default::default()
        };
        self.push(sqe)
    }

    /// A connect's socket was created: start the connect.
    pub(super) fn on_socket_cqe(&mut self, i: u32, res: i32, out: &mut Completions) {
        if res < 0 {
            return self.finish(i, res, out);
        }
        if self.slots[i as usize].cancel_requested {
            // Cancelled while the socket was being created: do not start
            // the connect, which nothing would cancel.
            if self.slots[i as usize].sock_closed {
                return self.finish(i, -ECANCELED, out);
            }
            return self.release_socket(i, -ECANCELED);
        }
        self.slots[i as usize].state = State::ConnectStep;
        self.slots[i as usize].phase = 0;
        let sqe = self.connect_sqe(i);
        self.push(sqe)
    }
}
