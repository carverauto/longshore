// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The kernel transport as a [`Rig`].

use super::Rig;
use crate::check::{fail, must, sizes};
use crate::transport::{Happened, Span, Transport};
use crate::{Addr, Config, KernelTransport};
use longshore_common::io::{BufId, Completions, Io, Limits, Op, Token};
use std::net::SocketAddr;

/// The kernel transport on a backend of the caller's making.
pub struct KernelRig<IO: Io> {
    t: KernelTransport<IO>,
    io: IO,
    out: Completions,
    cfg: Config,
    addr: SocketAddr,
    ticking: bool,
    /// Timers that hold the backend's room for operations.
    fillers: u32,
}

/// The rig's own timer, outside the transport's tokens, and the first of
/// the timers that fill the backend.
const TICK: Token = Token::new(1 << 40);
const FILL: u64 = 1 << 41;

impl<IO: Io> KernelRig<IO> {
    /// A rig whose listener listens at `addr`.
    pub fn new(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) -> KernelRig<IO> {
        let (limits, cfg) = must(Config::plan(Limits::BASE, sizes(2, 2, 1)), "plan");
        let cfg = cfg.resolved(&limits);
        let mut t = KernelTransport::new(&cfg);
        t.attach(&cfg);
        KernelRig {
            t,
            io: make(limits.with_ops_max(limits.ops_max() + 2)),
            out: Completions::new(32),
            cfg,
            addr,
            ticking: false,
            fillers: 0,
        }
    }
}

impl<IO: Io> Rig for KernelRig<IO> {
    type T = KernelTransport<IO>;

    fn parts(&mut self) -> (&mut KernelTransport<IO>, &mut IO) {
        (&mut self.t, &mut self.io)
    }

    fn turn(&mut self, seen: &mut Vec<Happened>) {
        if !self.ticking && self.fillers == 0 {
            // So that time passes when nothing else does.
            must(
                self.io.submit(TICK, Op::Timeout { nanos: 250_000 }),
                "timer",
            );
            self.ticking = true;
        }
        must(self.io.tick(true, &mut self.out), "tick");
        while let Some(c) = self.out.pop() {
            if c.token() == TICK {
                self.ticking = false;
                continue;
            }
            if c.token().get() >= FILL {
                self.fillers -= 1;
                continue;
            }
            match self.t.translate(&mut self.io, c) {
                Some(Happened::Nothing) => {}
                Some(happened) => seen.push(happened),
                None => fail(format_args!(
                    "a completion the transport did not know: {c:?}"
                )),
            }
        }
    }

    fn span(&self, i: u32, len: u32) -> Span {
        Span::new(BufId::new(self.cfg.frame_first().get() + i), 0, len)
    }

    fn addr(&self) -> Addr {
        Addr::Ip(self.addr)
    }

    fn starve(&mut self) -> bool {
        // Timers, until the backend takes no more operations.
        let fill = |at: u32| {
            (
                Token::new(FILL + u64::from(at)),
                Op::Timeout { nanos: 2_000_000 },
            )
        };
        while self.fillers < 4096 {
            let (token, op) = fill(self.fillers);
            if self.io.submit(token, op).is_err() {
                return true;
            }
            self.fillers += 1;
        }
        fail(format_args!("the backend never ran out of room"))
    }

    fn relieve(&mut self, seen: &mut Vec<Happened>) {
        for _ in 0..20_000 {
            if self.fillers == 0 {
                return;
            }
            self.turn(seen);
        }
        fail(format_args!(
            "the timers that filled the backend never ended"
        ));
    }
}
