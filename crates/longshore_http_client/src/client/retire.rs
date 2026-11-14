// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Taking connections out of reuse, for an owner that prefers some of a
//! host's addresses over others.

use super::{Client, State};

impl Client {
    /// Stop reusing the connections to `host_id` whose measured segment
    /// size is below `floor`. Each finishes the request it is serving and
    /// is replaced the next time it would be picked. Connections that were
    /// not measured are kept. Returns how many were retired by this call.
    pub fn retire_below(&mut self, host_id: u16, floor: u32) -> u32 {
        let mut retired = 0;
        for c in self.conns.iter_mut() {
            let small = c.segment > 0 && c.segment < floor;
            if c.host_id == host_id && c.state != State::Closed && small && !c.retired {
                c.retired = true;
                retired += 1;
            }
        }
        retired
    }

    /// Stop reusing the connections to `host_id` that were opened without
    /// their segment size being measured, so that their replacements are
    /// measured. Returns how many were retired by this call.
    pub fn retire_unmeasured(&mut self, host_id: u16) -> u32 {
        let mut retired = 0;
        for c in self.conns.iter_mut() {
            if c.host_id == host_id && c.state != State::Closed && c.segment == 0 && !c.retired {
                c.retired = true;
                retired += 1;
            }
        }
        retired
    }

    /// Stop reusing up to `n` of the open connections to `host_id`, so that
    /// their replacements try other addresses. Returns how many were
    /// retired by this call.
    pub fn retire_some(&mut self, host_id: u16, n: u32) -> u32 {
        let mut retired = 0;
        for c in self.conns.iter_mut() {
            if retired == n {
                break;
            }
            if c.host_id == host_id && c.state != State::Closed && !c.retired {
                c.retired = true;
                retired += 1;
            }
        }
        retired
    }
}
