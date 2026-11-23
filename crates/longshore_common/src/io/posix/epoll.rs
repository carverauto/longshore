// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `epoll` poller and `eventfd` waker for Linux.

#![cfg(target_os = "linux")]

use super::abi_linux::*;
use super::SockSlot;
use crate::io::Wake;
use core::ffi::c_int;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const WAKE_TAG: u64 = u64::MAX;
const EVENTS_CAP: usize = 64;

/// The `maxevents` argument: never more than the event buffer holds.
fn max_events() -> c_int {
    c_int::try_from(EVENTS_CAP).unwrap_or(c_int::MAX)
}

struct EpollWaker {
    woken: Arc<AtomicBool>,
    fd_dup: i32,
}

impl Wake for EpollWaker {
    fn wake(&self) {
        self.woken.store(true, Ordering::Release);
        let one: u64 = 1;
        let _: isize = unsafe { write(self.fd_dup, (&one as *const u64).cast(), 8) };
    }
}

impl Drop for EpollWaker {
    fn drop(&mut self) {
        if self.fd_dup >= 0 {
            let _: i32 = unsafe { close(self.fd_dup) };
        }
    }
}

pub(super) struct Poller {
    epfd: i32,
    wake_fd: i32,
    events: Box<[epoll_event; EVENTS_CAP]>,
}

impl Poller {
    pub(super) fn new() -> io::Result<Self> {
        let epfd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if epfd < 0 {
            return Err(io::Error::from_raw_os_error(os_errno()));
        }
        let wake_fd = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if wake_fd < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            let _: i32 = unsafe { close(epfd) };
            return Err(err);
        }
        let mut ev = epoll_event {
            events: EPOLLIN,
            data: WAKE_TAG,
        };
        if unsafe { epoll_ctl(epfd, EPOLL_CTL_ADD, wake_fd, &mut ev) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            unsafe {
                let _: i32 = close(wake_fd);
                let _: i32 = close(epfd);
            }
            return Err(err);
        }
        Ok(Self {
            epfd,
            wake_fd,
            events: Box::new([epoll_event::default(); EVENTS_CAP]),
        })
    }

    pub(super) fn waker(&self, woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        let fd_dup = unsafe { fcntl(self.wake_fd, F_DUPFD_CLOEXEC, 0) };
        (fd_dup >= 0).then(|| Arc::new(EpollWaker { woken, fd_dup }) as Arc<dyn Wake>)
    }

    fn set_mask(&self, slot: usize, s: &mut SockSlot, mask: u32) {
        let cur =
            (if s.armed_read { EPOLLIN } else { 0 }) | (if s.armed_write { EPOLLOUT } else { 0 });
        if cur == mask || s.fd < 0 {
            return;
        }
        let op = if cur == 0 {
            EPOLL_CTL_ADD
        } else if mask == 0 {
            EPOLL_CTL_DEL
        } else {
            EPOLL_CTL_MOD
        };
        let mut ev = epoll_event {
            events: mask,
            data: slot as u64,
        };
        let ev_ptr = if op == EPOLL_CTL_DEL {
            core::ptr::null_mut()
        } else {
            &mut ev as *mut epoll_event
        };
        if unsafe { epoll_ctl(self.epfd, op, s.fd, ev_ptr) } == 0 {
            s.armed_read = mask & EPOLLIN != 0;
            s.armed_write = mask & EPOLLOUT != 0;
        }
    }

    pub(super) fn arm_read(&mut self, slot: usize, s: &mut SockSlot) {
        let mask = EPOLLIN | if s.armed_write { EPOLLOUT } else { 0 };
        self.set_mask(slot, s, mask);
    }

    pub(super) fn arm_write(&mut self, slot: usize, s: &mut SockSlot) {
        let mask = EPOLLOUT | if s.armed_read { EPOLLIN } else { 0 };
        self.set_mask(slot, s, mask);
    }

    pub(super) fn before_close(&mut self, s: &mut SockSlot) {
        if s.fd >= 0 && (s.armed_read || s.armed_write) {
            let _: i32 =
                unsafe { epoll_ctl(self.epfd, EPOLL_CTL_DEL, s.fd, core::ptr::null_mut()) };
        }
        s.armed_read = false;
        s.armed_write = false;
    }

    pub(super) fn poll(&mut self, socks: &mut [SockSlot]) {
        loop {
            let n = unsafe { epoll_wait(self.epfd, self.events.as_mut_ptr(), max_events(), 0) };
            let Ok(n) = usize::try_from(n) else {
                if os_errno() == OS_EINTR {
                    continue;
                }
                return;
            };
            self.apply(n, socks);
            if n < EVENTS_CAP {
                return;
            }
        }
    }

    pub(super) fn wait(
        &mut self,
        socks: &mut [SockSlot],
        deadline: Option<u64>,
        now_fn: impl Fn() -> u64,
    ) -> io::Result<()> {
        for (slot, s) in socks.iter_mut().enumerate() {
            if s.fd >= 0 {
                let want = (if s.want_read && !s.read_ready {
                    EPOLLIN
                } else {
                    0
                }) | (if s.want_write && !s.write_ready {
                    EPOLLOUT
                } else {
                    0
                });
                self.set_mask(slot, s, want);
            }
        }
        loop {
            let timeout_ms = match deadline {
                Some(dl) => {
                    let rem = dl.saturating_sub(now_fn());
                    if rem == 0 {
                        return Ok(());
                    }
                    let ms = rem.div_ceil(1_000_000);
                    ms.clamp(1, i32::MAX as u64) as i32
                }
                None => -1,
            };
            let n = unsafe {
                epoll_wait(
                    self.epfd,
                    self.events.as_mut_ptr(),
                    max_events(),
                    timeout_ms,
                )
            };
            let Ok(n) = usize::try_from(n) else {
                let err = os_errno();
                if err == OS_EINTR {
                    continue;
                }
                return Err(io::Error::from_raw_os_error(err));
            };
            if n > 0 {
                self.apply(n, socks);
            }
            return Ok(());
        }
    }

    fn apply(&mut self, n: usize, socks: &mut [SockSlot]) {
        for i in 0..n {
            let ev = self.events[i];
            let tag = ev.data_u64();
            let bits = ev.events_bits();
            if tag == WAKE_TAG {
                let mut val = 0u64;
                let _: isize = unsafe { read(self.wake_fd, (&mut val as *mut u64).cast(), 8) };
                continue;
            }
            let Some(s) = socks.get_mut(tag as usize) else {
                continue;
            };
            if s.fd < 0 {
                continue;
            }
            let err_hup = EPOLLERR | EPOLLHUP;
            if bits & (EPOLLIN | EPOLLRDHUP | err_hup) != 0 {
                s.read_ready = true;
            }
            if bits & (EPOLLOUT | err_hup) != 0 {
                s.write_ready = true;
            }
            // Disarm any direction whose readiness is now remembered so
            // level-triggered epoll does not busy-spin while completions drain.
            let next_mask = (if s.armed_read && !s.read_ready {
                EPOLLIN
            } else {
                0
            }) | (if s.armed_write && !s.write_ready {
                EPOLLOUT
            } else {
                0
            });
            self.set_mask(tag as usize, s, next_mask);
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        if self.wake_fd >= 0 {
            let _: i32 = unsafe { close(self.wake_fd) };
        }
        if self.epfd >= 0 {
            let _: i32 = unsafe { close(self.epfd) };
        }
    }
}
