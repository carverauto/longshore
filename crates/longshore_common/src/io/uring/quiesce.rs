// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Ending every operation in flight before [`UringIo`] gives up its memory.

use super::*;

impl UringIo {
    /// Cancel every operation in flight and reap it, so the kernel touches
    /// no buffer or slot afterwards. Completions are discarded. True once
    /// nothing is in flight; false if the ring fails or something has not
    /// ended within five seconds (a zero-copy send whose data the peer never
    /// acknowledges can hold its buffer that long). Dropping calls this.
    pub fn quiesce(&mut self) -> bool {
        self.ready.clear(&mut self.slots, &mut self.free);
        if self.in_flight == 0 {
            return true;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Operations of several steps do not start their next step.
        for s in self.slots.iter_mut() {
            s.cancel_requested = true;
        }
        let mut timing = false;
        let mut out = Completions::new(8);
        while self.in_flight > 0 {
            if std::time::Instant::now() > deadline {
                return false;
            }
            if !self.cancel_in_flight() {
                return false;
            }
            if !timing {
                let timer = io_uring_sqe {
                    opcode: IORING_OP_TIMEOUT,
                    addr: &*self.quiet_ts as *const timespec as u64,
                    len: 1,
                    user_data: QUIESCE | 1,
                    ..Default::default()
                };
                if !self.push_flushing(timer) {
                    return false;
                }
                timing = true;
            }
            if self.ring.enter(1).is_err() {
                return false;
            }
            while let Some(c) = self.reap() {
                match c.user_data {
                    QUIESCE | INTERNAL_CANCEL | INTERNAL_SHUTDOWN => {}
                    t if t == QUIESCE | 1 => timing = false,
                    _ => {
                        self.on_cqe(c, &mut out);
                        while out.pop().is_some() {}
                    }
                }
            }
        }
        true
    }

    /// Asks the kernel to end every operation in flight. False if the ring
    /// fails.
    fn cancel_in_flight(&mut self) -> bool {
        for k in 0..self.slots.len() {
            let s = &self.slots[k];
            let Some(opcode) = quiesce_opcode(s.state) else {
                continue;
            };
            let cancel = io_uring_sqe {
                opcode,
                fd: -1,
                addr: k as u64 | s.phase,
                user_data: QUIESCE,
                ..Default::default()
            };
            if !self.push_flushing(cancel) {
                return false;
            }
        }
        true
    }
}

/// How quiescing ends an operation in `state`: a timer is removed, anything
/// else in flight cancelled. `None` when nothing is in flight or the
/// operation ends without help (closes, releases, a zero-copy send waiting
/// for its notification).
fn quiesce_opcode(state: State) -> Option<u8> {
    match state {
        State::Free
        | State::Close
        | State::FileClose
        | State::FileSlotClose
        | State::ConnectRelease
        | State::AwaitNotif => None,
        State::Timer => Some(IORING_OP_TIMEOUT_REMOVE),
        State::InFlight
        | State::Connecting
        | State::ConnectStep
        | State::Accept
        | State::ArmedAccept
        | State::ArmedRecv
        | State::SendZc
        | State::Wake
        | State::AwaitResult
        | State::FileOpen
        | State::FileRead
        | State::FileSlotOp
        | State::FileSlotOpen => Some(IORING_OP_ASYNC_CANCEL),
    }
}
