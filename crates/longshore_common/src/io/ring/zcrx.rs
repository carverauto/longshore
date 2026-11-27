// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The ring's calls for zero-copy receive: whether the kernel has it,
//! registering a receive queue with an area, and asking the kernel to
//! read the ring of returns.

use super::Ring;
use crate::io::sys::*;
use core::ffi::c_void;
use std::io;

impl Ring {
    /// Whether the kernel has operation `op`.
    pub fn has_op(&mut self, op: u8) -> io::Result<bool> {
        let none = io_uring_probe_op::default();
        let mut probe = Box::new(io_uring_probe {
            last_op: 0,
            ops_len: 0,
            resv: 0,
            resv2: [0; 3],
            ops: [none; 256],
        });
        // The kernel writes what it knows into it.
        let arg: *mut io_uring_probe = &raw mut *probe;
        self.register(IORING_REGISTER_PROBE, arg.cast_const().cast(), 256)?;
        let known = usize::from(probe.ops_len).min(probe.ops.len());
        let found = probe.ops[..known].iter().find(|o| o.op == op);
        Ok(found.is_some_and(|o| o.flags & IO_URING_OP_SUPPORTED != 0))
    }

    /// Register the receive queue `reg` names with its area and its ring
    /// of returns; the kernel fills in the rest of `reg` and of the area's
    /// description `reg` points to. `Err` carries the errno.
    pub fn register_zcrx(&mut self, reg: &mut io_uring_zcrx_ifq_reg) -> Result<(), i32> {
        let arg: *const io_uring_zcrx_ifq_reg = reg;
        self.register_raw(IORING_REGISTER_ZCRX_IFQ, arg.cast::<c_void>(), 1)
    }

    /// Ask the kernel to read the ring of returns of registration `id`
    /// now. `Err` carries the errno. Allocates nothing.
    pub fn flush_zcrx(&mut self, id: u32) -> Result<(), i32> {
        let ctrl = zcrx_ctrl {
            zcrx_id: id,
            op: ZCRX_CTRL_FLUSH_RQ,
            ..Default::default()
        };
        let arg: *const zcrx_ctrl = &raw const ctrl;
        self.register_raw(IORING_REGISTER_ZCRX_CTRL, arg.cast::<c_void>(), 0)
    }
}
