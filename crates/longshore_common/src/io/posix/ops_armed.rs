// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Armed accept/receive, buffer-ring free list, and Unix-domain connect for [`Posix`].

#[cfg(target_os = "linux")]
use super::abi_linux::*;
#[cfg(target_os = "macos")]
use super::abi_macos::*;
use super::errno::{self, to_linux_errno};
use super::sock::{accept_socket, open_socket, sockaddr_un_from};
use super::{Posix, SockKind};
use crate::io::engine::Attempt;
use crate::io::{count_result, BufId, Limits, Sock};
use core::ffi::c_void;
use core::mem::size_of;

impl Posix {
    pub(super) fn give_back_ring(&mut self, id: BufId) {
        let idx = (id.0 - self.ring_base) as usize;
        assert!(!self.ring_in_free[idx], "ring buffer already in ring");
        self.ring_in_free[idx] = true;
        self.ring_free[self.ring_free_len] = idx as u16;
        self.ring_free_len += 1;
    }

    pub(super) fn attempt_accept_armed(&mut self, listener: Sock) -> Option<Attempt> {
        let lidx = listener.0 as usize;
        let lfd = self.socks[lidx].fd;
        if lfd < 0 {
            return Some(Attempt::single(-errno::linux::EBADF));
        }
        match accept_socket(lfd) {
            Ok(new_fd) => {
                let end = self.accept_start + self.accept_len;
                let slot = (self.accept_start..end)
                    .find(|&k| self.socks[k as usize].kind == SockKind::Free);
                if let Some(slot_idx) = slot {
                    self.socks[lidx].read_ready = true;
                    let s = &mut self.socks[slot_idx as usize];
                    s.reset();
                    s.fd = new_fd;
                    s.kind = SockKind::Open;
                    Some(Attempt {
                        result: count_result(slot_idx),
                        more: true,
                        buf: None,
                    })
                } else {
                    let lin = linger {
                        l_onoff: 1,
                        l_linger: 0,
                    };
                    unsafe {
                        let _: i32 = setsockopt(
                            new_fd,
                            SOL_SOCKET,
                            SO_LINGER,
                            (&lin as *const linger).cast(),
                            size_of::<linger>() as u32,
                        );
                        let _: i32 = close(new_fd);
                    }
                    Some(Attempt::single(-errno::linux::ENFILE))
                }
            }
            Err(e) if e == OS_EAGAIN || e == OS_EWOULDBLOCK => {
                self.socks[lidx].read_ready = false;
                self.poller.arm_read(lidx, &mut self.socks[lidx]);
                None
            }
            Err(e) => Some(Attempt::single(to_linux_errno(e))),
        }
    }

    pub(super) fn attempt_recv_armed(
        &mut self,
        sock: Sock,
        pool: &mut [u8],
        limits: &Limits,
    ) -> Option<Attempt> {
        let idx = sock.0 as usize;
        let fd = self.socks[idx].fd;
        if fd < 0 {
            return Some(Attempt::single(-errno::linux::EBADF));
        }
        if self.ring_free_len == 0 {
            let mut byte = 0u8;
            loop {
                let n = unsafe {
                    recv(
                        fd,
                        (&mut byte as *mut u8).cast::<c_void>(),
                        1,
                        MSG_DONTWAIT | MSG_PEEK,
                    )
                };
                if n == 0 {
                    return Some(Attempt::single(0));
                }
                if n > 0 {
                    return Some(Attempt::single(-errno::linux::ENOBUFS));
                }
                let e = os_errno();
                if e == OS_EINTR {
                    continue;
                }
                if e == OS_EAGAIN || e == OS_EWOULDBLOCK {
                    self.socks[idx].read_ready = false;
                    self.poller.arm_read(idx, &mut self.socks[idx]);
                    return None;
                }
                return Some(Attempt::single(to_linux_errno(e)));
            }
        }

        let ring_idx = self.ring_free[self.ring_free_len - 1];
        let buf_id = BufId(self.ring_base + u32::from(ring_idx));
        let range = limits.range(buf_id, 0, limits.ring_buffer_size);
        let ptr: *mut c_void = pool[range.clone()].as_mut_ptr().cast();
        let len = range.len();
        loop {
            let n = unsafe { recv(fd, ptr, len, MSG_DONTWAIT) };
            if n > 0 {
                self.socks[idx].read_ready = true;
                self.ring_free_len -= 1;
                self.ring_in_free[ring_idx as usize] = false;
                return Some(Attempt {
                    result: n as i32,
                    more: true,
                    buf: Some(buf_id),
                });
            }
            if n == 0 {
                return Some(Attempt::single(0));
            }
            let e = os_errno();
            if e == OS_EINTR {
                continue;
            }
            if e == OS_EAGAIN || e == OS_EWOULDBLOCK {
                self.socks[idx].read_ready = false;
                self.poller.arm_read(idx, &mut self.socks[idx]);
                return None;
            }
            return Some(Attempt::single(to_linux_errno(e)));
        }
    }

    pub(super) fn connect_unix(&mut self, sock: Sock, path_with_nul: &[u8]) -> Option<i32> {
        let idx = sock.0 as usize;
        if self.socks[idx].kind == SockKind::Connecting {
            return self.finish_connecting(idx);
        }
        let (sa, sa_len) = match sockaddr_un_from(path_with_nul) {
            Ok(v) => v,
            Err(e) => return Some(to_linux_errno(e)),
        };
        let fd = match open_socket(AF_UNIX, SOCK_STREAM) {
            Ok(fd) => fd,
            Err(e) => return Some(to_linux_errno(e)),
        };
        if unsafe { connect(fd, (&sa as *const sockaddr_storage).cast(), sa_len) } == 0 {
            self.socks[idx].reset();
            self.socks[idx].fd = fd;
            self.socks[idx].kind = SockKind::Open;
            return Some(0);
        }
        let e = os_errno();
        if e == OS_EINPROGRESS || e == OS_EINTR {
            self.socks[idx].reset();
            self.socks[idx].fd = fd;
            self.socks[idx].kind = SockKind::Connecting;
            self.poller.arm_write(idx, &mut self.socks[idx]);
            return None;
        }
        let _: i32 = unsafe { close(fd) };
        Some(to_linux_errno(e))
    }
}
