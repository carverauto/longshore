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
        let fd = unsafe { socket(family as i32, kind, 0) };
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
            if let Err(errno) =
                super::backend::apply_tcp_options(fd, self.no_delay, self.keep_alive)
            {
                unsafe { close(fd) };
                self.slots[i as usize].state = State::Free;
                self.ready.push(i, Completion::new(token, -errno));
                return None;
            }
        }
        if let Err(errno) = self.ring.update_file_raw(sock.0, fd) {
            unsafe { close(fd) };
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
            if self.measure_segments {
                let option = |level, name| {
                    let mut value: i32 = 0;
                    let mut len = core::mem::size_of::<i32>() as u32;
                    let r = unsafe {
                        getsockopt(fd, level, name, (&mut value as *mut i32).cast(), &mut len)
                    };
                    if r == 0 && value > 0 {
                        value as u32
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
        let id = BufId(self.limits.ring_base() + ring_id as u32);
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
            CqeDispatch::SendZc => {
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
                return self.finish(i, c.res, out);
            }
            CqeDispatch::Armed { more, read_buf } => {
                let buf = if read_buf {
                    self.decode_cqe_buf(&c)
                } else {
                    if c.res >= 0 && (c.res as u32) < self.limits.sockets_max {
                        self.sock_stream[c.res as usize] = true;
                    }
                    None
                };
                self.push_armed(i, c.res, more, buf, out);
                return;
            }
            CqeDispatch::Single => {}
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
                if self.slots[i as usize].sock_closed {
                    return self.finish(i, -ECANCELED, out);
                }
                return self.release_socket(i, -ECANCELED);
            }
            self.slots[i as usize].state = State::ConnectStep;
            self.slots[i as usize].phase = 0;
            let sqe = self.connect_sqe(i);
            return self.push(sqe);
        }
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
    Some(BufId(base + ring_id as u32))
}
