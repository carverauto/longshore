// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `kqueue` poller and `EVFILT_USER` waker for macOS.

#![cfg(target_os = "macos")]

use super::abi_macos::*;
use super::SockSlot;
use crate::io::Wake;
use core::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const WAKE_IDENT: usize = 1;
const EVENTS_CAP: usize = 64;

struct KqueueWaker {
    woken: Arc<AtomicBool>,
    kq_dup: i32,
}

impl Wake for KqueueWaker {
    fn wake(&self) {
        self.woken.store(true, Ordering::Release);
        let kev = kevent {
            ident: WAKE_IDENT,
            filter: EVFILT_USER,
            flags: 0,
            fflags: NOTE_TRIGGER,
            data: 0,
            udata: core::ptr::null_mut(),
        };
        unsafe {
            kevent(
                self.kq_dup,
                &kev,
                1,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
            );
        }
    }
}

impl Drop for KqueueWaker {
    fn drop(&mut self) {
        if self.kq_dup >= 0 {
            unsafe { close(self.kq_dup) };
        }
    }
}

pub(super) struct Poller {
    kq: i32,
    events: Box<[kevent; EVENTS_CAP]>,
}

impl Poller {
    pub(super) fn new() -> io::Result<Self> {
        let kq = unsafe { kqueue() };
        if kq < 0 {
            return Err(io::Error::from_raw_os_error(os_errno()));
        }
        if unsafe { fcntl(kq, F_SETFD, FD_CLOEXEC) } < 0 {
            let err = io::Error::from_raw_os_error(os_errno());
            unsafe { close(kq) };
            return Err(err);
        }
        let wake_reg = kevent {
            ident: WAKE_IDENT,
            filter: EVFILT_USER,
            flags: EV_ADD | EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: core::ptr::null_mut(),
        };
        if unsafe {
            kevent(
                kq,
                &wake_reg,
                1,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
            )
        } < 0
        {
            let err = io::Error::from_raw_os_error(os_errno());
            unsafe { close(kq) };
            return Err(err);
        }
        Ok(Self {
            kq,
            events: Box::new([kevent::default(); EVENTS_CAP]),
        })
    }

    pub(super) fn waker(&self, woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        let kq_dup = unsafe { fcntl(self.kq, F_DUPFD_CLOEXEC, 0) };
        (kq_dup >= 0).then(|| Arc::new(KqueueWaker { woken, kq_dup }) as Arc<dyn Wake>)
    }

    pub(super) fn arm_read(&mut self, slot: usize, s: &mut SockSlot) {
        if s.fd < 0 || s.armed_read {
            return;
        }
        let kev = kevent {
            ident: s.fd as usize,
            filter: EVFILT_READ,
            flags: EV_ADD | EV_ENABLE | EV_ONESHOT,
            fflags: 0,
            data: 0,
            udata: slot as *mut c_void,
        };
        if unsafe {
            kevent(
                self.kq,
                &kev,
                1,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
            )
        } == 0
        {
            s.armed_read = true;
        }
    }

    pub(super) fn arm_write(&mut self, slot: usize, s: &mut SockSlot) {
        if s.fd < 0 || s.armed_write {
            return;
        }
        let kev = kevent {
            ident: s.fd as usize,
            filter: EVFILT_WRITE,
            flags: EV_ADD | EV_ENABLE | EV_ONESHOT,
            fflags: 0,
            data: 0,
            udata: slot as *mut c_void,
        };
        if unsafe {
            kevent(
                self.kq,
                &kev,
                1,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
            )
        } == 0
        {
            s.armed_write = true;
        }
    }

    pub(super) fn before_close(&mut self, _s: &mut SockSlot) {
        // Closing the fd automatically drops any registered knotes.
    }

    pub(super) fn poll(&mut self, socks: &mut [SockSlot]) {
        let zero = timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            let n = unsafe {
                kevent(
                    self.kq,
                    core::ptr::null(),
                    0,
                    self.events.as_mut_ptr(),
                    EVENTS_CAP as i32,
                    &zero,
                )
            };
            if n < 0 {
                if os_errno() == OS_EINTR {
                    continue;
                }
                return;
            }
            self.apply(n as usize, socks);
            if (n as usize) < EVENTS_CAP {
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
        loop {
            let mut ts = timespec::default();
            let ts_ptr = match deadline {
                Some(dl) => {
                    let rem = dl.saturating_sub(now_fn());
                    if rem == 0 {
                        return Ok(());
                    }
                    ts.tv_sec = (rem / 1_000_000_000) as i64;
                    ts.tv_nsec = (rem % 1_000_000_000) as i64;
                    &ts as *const timespec
                }
                None => core::ptr::null(),
            };
            let n = unsafe {
                kevent(
                    self.kq,
                    core::ptr::null(),
                    0,
                    self.events.as_mut_ptr(),
                    EVENTS_CAP as i32,
                    ts_ptr,
                )
            };
            if n < 0 {
                let err = os_errno();
                if err == OS_EINTR {
                    continue;
                }
                return Err(io::Error::from_raw_os_error(err));
            }
            if n > 0 {
                self.apply(n as usize, socks);
            }
            return Ok(());
        }
    }

    fn apply(&mut self, n: usize, socks: &mut [SockSlot]) {
        for ev in &self.events[..n] {
            if ev.filter == EVFILT_USER {
                continue;
            }
            let idx = ev.udata as usize;
            if let Some(s) = socks.get_mut(idx) {
                if s.fd >= 0 && s.fd as usize == ev.ident {
                    if ev.filter == EVFILT_READ {
                        s.armed_read = false;
                        s.read_ready = true;
                    } else if ev.filter == EVFILT_WRITE {
                        s.armed_write = false;
                        s.write_ready = true;
                    }
                }
            }
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        if self.kq >= 0 {
            unsafe { close(self.kq) };
        }
    }
}
