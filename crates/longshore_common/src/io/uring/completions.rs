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
        sqe.off = u64::from(s.addr_len);
        sqe.user_data = u64::from(i);
        sqe
    }

    /// Open the socket of a connect with a descriptor of our own, so socket
    /// options can be set and segment sizes can be read later, and register
    /// it in the slot. When `no_delay` or `keep_alive` is configured, failure
    /// completes with the negative errno instead of falling back to
    /// `IORING_OP_SOCKET`.
    pub(super) fn connect_with_fd(
        &mut self,
        i: u32,
        sock: Sock,
        family: u16,
        token: Token,
    ) -> Option<io_uring_sqe> {
        let opts = self.no_delay || self.keep_alive;
        let kind = (SOCK_STREAM | SOCK_CLOEXEC) as i32;
        let fd = unsafe { socket(i32::from(family), kind, 0) };
        if fd < 0 {
            if opts {
                let errno = io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(crate::io::EINVAL);
                self.slots[i as usize].state = State::Free;
                self.ready.push(i, Completion::new(token, -errno));
            }
            return None;
        }
        if opts {
            if let Err(errno) = super::backend::apply_tcp_options(
                fd,
                self.no_delay,
                self.keep_alive,
                self.keep_timing,
            ) {
                let _: i32 = unsafe { close(fd) };
                self.slots[i as usize].state = State::Free;
                self.ready.push(i, Completion::new(token, -errno));
                return None;
            }
        }
        if let Err(errno) = self.ring.update_file_raw(sock.0, fd) {
            let _: i32 = unsafe { close(fd) };
            if opts {
                self.slots[i as usize].state = State::Free;
                self.ready.push(i, Completion::new(token, -errno));
            }
            return None;
        }
        let s = &mut self.slots[i as usize];
        s.probe_fd = fd;
        s.state = State::ConnectStep;
        s.phase = 0;
        Some(self.connect_sqe(i))
    }

    /// The connect of a measured/optioned socket has ended. Read the socket
    /// options (and segment sizes if asked) if it succeeded, and close our
    /// descriptor either way: the registered table keeps the socket.
    fn end_measure(&mut self, i: u32, result: i32) -> i32 {
        let s = &mut self.slots[i as usize];
        let fd = core::mem::replace(&mut s.probe_fd, -1);
        if fd < 0 {
            return result;
        }
        let mut measured = result;
        if result == 0 {
            self.last_connect_opts = Some(super::backend::read_tcp_options(fd));
            self.last_connect_timing = Some(super::backend::read_keep_timing(fd));
            if self.measure_segments {
                let option = |level, name| {
                    let mut value: i32 = 0;
                    let mut len = core::mem::size_of::<i32>() as u32;
                    let r = unsafe {
                        getsockopt(fd, level, name, (&mut value as *mut i32).cast(), &mut len)
                    };
                    if r == 0 {
                        u32::try_from(value).unwrap_or(0)
                    } else {
                        0
                    }
                };
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
        }
        let _: i32 = unsafe { close(fd) };
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
            user_data: PHASE_SOCK_RELEASE | u64::from(i),
            ..Default::default()
        };
        self.push(sqe)
    }

    pub(super) fn finish(&mut self, i: u32, result: i32, out: &mut Completions) {
        let s = &mut self.slots[i as usize];
        out.push(Completion::new(Token(s.token), result));
        s.state = State::Free;
        self.free.push(i);
    }

    fn push_armed(
        &mut self,
        i: u32,
        result: i32,
        more: bool,
        buf: Option<BufId>,
        out: &mut Completions,
    ) {
        let s = &mut self.slots[i as usize];
        out.push(Completion {
            token: Token(s.token),
            result,
            more,
            buf,
        });
        if !more {
            s.state = State::Free;
            self.free.push(i);
        }
    }

    fn decode_cqe_buf(&mut self, c: &io_uring_cqe) -> Option<BufId> {
        let base = self.limits.ring_base();
        let mut empty = None;
        let buf = decode_ring_buf(c.flags, c.res, base, |id| empty = Some(id));
        if let Some(id) = empty {
            self.return_ring_buf(id);
        } else if let Some(id) = buf {
            self.ring_in_ring[(id.0 - base) as usize] = false;
        }
        buf
    }

    pub(super) fn return_ring_buf(&mut self, ring_id: u16) {
        let id = BufId(self.limits.ring_base() + u32::from(ring_id));
        let len = self.limits.ring_buffer_size;
        let addr = self.buf_ptr(id, 0, len);
        self.ring_in_ring[ring_id as usize] = true;
        if let Some(br) = self.buf_ring.as_mut() {
            br.push_buf(addr, len, ring_id);
        }
    }

    pub(super) fn on_cqe(&mut self, c: io_uring_cqe, out: &mut Completions) {
        if matches!(c.user_data, INTERNAL_CANCEL | INTERNAL_SHUTDOWN | QUIESCE) {
            return;
        }
        let i = c.user_data as u32;
        let state = self.slots[i as usize].state;
        match dispatch_cqe(state, c.flags) {
            CqeDispatch::SendZc => self.on_send_zc_cqe(i, state, &c, out),
            CqeDispatch::Armed { more, read_buf } => self.on_armed_cqe(i, &c, more, read_buf, out),
            CqeDispatch::Single => self.on_single_cqe(i, state, &c, out),
        }
    }

    /// A zero-copy send reports its result and, separately, the notification
    /// that the kernel no longer reads the buffer; it finishes on the later.
    #[inline]
    fn on_send_zc_cqe(&mut self, i: u32, state: State, c: &io_uring_cqe, out: &mut Completions) {
        if c.flags & IORING_CQE_F_NOTIF != 0 {
            if state == State::AwaitNotif {
                let r = self.slots[i as usize].result;
                self.finish(i, r, out);
            } else {
                self.slots[i as usize].state = State::AwaitResult;
            }
            return;
        }
        if c.flags & IORING_CQE_F_MORE != 0 {
            let s = &mut self.slots[i as usize];
            if s.state == State::AwaitResult {
                return self.finish(i, c.res, out);
            }
            s.result = c.res;
            s.state = State::AwaitNotif;
            return;
        }
        self.finish(i, c.res, out)
    }

    /// One completion of a multishot accept or receive.
    #[inline]
    fn on_armed_cqe(
        &mut self,
        i: u32,
        c: &io_uring_cqe,
        more: bool,
        read_buf: bool,
        out: &mut Completions,
    ) {
        let buf = if read_buf {
            self.decode_cqe_buf(c)
        } else {
            if let Ok(slot) = u32::try_from(c.res) {
                if slot < self.limits.sockets_max {
                    self.sock_peer[slot as usize] = None;
                    self.sock_stream[slot as usize] = true;
                }
            }
            None
        };
        self.push_armed(i, c.res, more, buf, out);
    }

    /// The completion of a one-shot operation, or of one phase of a
    /// multi-step one (file read, connect).
    #[inline]
    fn on_single_cqe(&mut self, i: u32, state: State, c: &io_uring_cqe, out: &mut Completions) {
        match c.user_data & (7 << 32) {
            PHASE_FILE_OPEN => self.on_file_open_cqe(i, c.res, out),
            PHASE_FILE_READ => self.on_file_read_cqe(i, c.res),
            PHASE_FILE_CLOSE | PHASE_SOCK_RELEASE => {
                let r = self.slots[i as usize].result;
                self.finish(i, r, out);
            }
            PHASE_SOCKET => self.on_socket_cqe(i, c.res, out),
            _ => self.on_op_cqe(i, state, c, out),
        }
    }

    /// The final completion of a one-shot operation.
    #[inline]
    fn on_op_cqe(&mut self, i: u32, state: State, c: &io_uring_cqe, out: &mut Completions) {
        if state == State::ConnectStep {
            let r = self.end_measure(i, c.res);
            let s = self.slots[i as usize];
            if r < 0 && !s.sock_closed {
                return self.release_socket(i, r);
            }
            if r >= 0 && !s.sock_closed && s.is_stream {
                self.sock_stream[s.sock as usize] = true;
            }
            return self.finish(i, r, out);
        }
        if state == State::Accept {
            let s = self.slots[i as usize];
            if c.res >= 0 {
                self.sock_stream[s.sock as usize] = true;
                self.sock_peer[s.sock as usize] = s.addr.to_socket_addr();
            }
        }
        if state == State::FileSlotOpen {
            let s = &self.slots[i as usize];
            self.files_table
                .on_open_done(s.file_slot, c.res >= 0, s.file_direct);
        } else if state == State::FileSlotClose {
            let slot = self.slots[i as usize].file_slot;
            self.files_table.on_close_done(slot, c.res != -ECANCELED);
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

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CqeDispatch {
    SendZc,
    Armed { more: bool, read_buf: bool },
    Single,
}

pub(super) fn dispatch_cqe(state: State, flags: u32) -> CqeDispatch {
    match state {
        State::SendZc | State::AwaitNotif | State::AwaitResult => CqeDispatch::SendZc,
        State::ArmedAccept => CqeDispatch::Armed {
            more: flags & IORING_CQE_F_MORE != 0,
            read_buf: false,
        },
        State::ArmedRecv => CqeDispatch::Armed {
            more: flags & IORING_CQE_F_MORE != 0,
            read_buf: true,
        },
        State::Free
        | State::InFlight
        | State::Connecting
        | State::ConnectStep
        | State::ConnectRelease
        | State::Accept
        | State::Timer
        | State::Wake
        | State::FileOpen
        | State::FileRead
        | State::FileClose
        | State::FileSlotOp
        | State::FileSlotOpen
        | State::FileSlotClose
        | State::Close => CqeDispatch::Single,
    }
}

pub(super) fn decode_ring_buf(
    flags: u32,
    res: i32,
    base: u32,
    on_empty: impl FnOnce(u16),
) -> Option<BufId> {
    if flags & IORING_CQE_F_BUFFER == 0 {
        return None;
    }
    let ring_id = (flags >> IORING_CQE_BUFFER_SHIFT) as u16;
    if res <= 0 {
        on_empty(ring_id);
        return None;
    }
    Some(BufId(base + u32::from(ring_id)))
}
