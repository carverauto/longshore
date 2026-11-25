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
                return self.prep_connect(i, op, sock, addr, token);
            }
            Op::ConnectUnix {
                sock,
                path_buf,
                path_off,
                path_len,
            } => self.prep_connect_unix(i, sock, (path_buf, path_off, path_len)),
            Op::Accept { listener, sock } => self.prep_accept(i, listener, sock),
            Op::ArmedAccept { listener } => self.prep_armed_accept(i, listener),
            Op::ArmedRecv { sock } => self.prep_armed_recv(i, sock),
            Op::Send {
                sock,
                buf,
                off,
                len,
                zero_copy,
            } => self.prep_send(i, sock, (buf, off, len), zero_copy),
            Op::Recv {
                sock,
                buf,
                off,
                len,
            } => self.prep_recv(i, sock, (buf, off, len), 0),
            Op::RecvNow {
                sock,
                buf,
                off,
                len,
            } => self.prep_recv(i, sock, (buf, off, len), MSG_DONTWAIT),
            Op::Shutdown { sock, how } => self.prep_shutdown(i, sock, how),
            Op::Close { sock } => self.prep_close(i, sock),
            Op::Cancel { target } => self.prep_cancel(i, target),
            Op::ReadFile {
                file,
                buf,
                off,
                len,
            } => self.prep_read_file(i, file, (buf, off, len)),
            Op::AwaitWake => self.prep_await_wake(i),
            Op::Timeout { nanos } => self.prep_timeout(i, nanos),
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

    /// Cancels the live operation submitted with `target`; `addr` stays
    /// `u64::MAX` (matching nothing) for a close or an unknown token.
    fn prep_cancel(&mut self, i: u32, target: Token) -> io_uring_sqe {
        let found = self
            .slots
            .iter()
            .position(|s| s.state != State::Free && s.token == target.0)
            .filter(|k| *k as u32 != i);
        let ud = match found {
            Some(k)
                if matches!(
                    self.slots[k].state,
                    State::Close | State::FileClose | State::FileSlotClose | State::ConnectRelease
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

    /// Opens a registered file; the read follows when the open completes.
    fn prep_read_file(
        &mut self,
        i: u32,
        file: FileId,
        (buf, off, len): (BufId, u32, u32),
    ) -> io_uring_sqe {
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
        (s.state, s.phase, s.file_addr, s.file_len) = (State::FileOpen, PHASE_FILE_OPEN, addr, len);
        io_uring_sqe {
            opcode: IORING_OP_OPENAT,
            fd: AT_FDCWD,
            addr: path,
            op_flags: O_RDONLY_CLOEXEC,
            user_data: PHASE_FILE_OPEN,
            ..Default::default()
        }
    }

    fn prep_await_wake(&mut self, i: u32) -> io_uring_sqe {
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

    #[inline]
    fn prep_timeout(&mut self, i: u32, nanos: u64) -> io_uring_sqe {
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
}
