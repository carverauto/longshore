// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `alloc_guard`-sealed suites for `Posix` echo and armed networking buffer rings.

use longshore::io::alloc_guard::sealed;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};
use std::net::SocketAddr;

#[cfg(unix)]
pub fn run_posix_echo_sealed() {
    use super::super::harness::posix_backend;
    use longshore::io::scenario::echo;

    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix backend");
    let mut out = Completions::new(16);
    let addr = "127.0.0.1:0".parse().expect("loopback addr");

    let stats = sealed(|| echo(&mut io, addr, 128 * 1024, 4096, false, &mut out));
    let stats = stats.expect("sealed posix echo");
    assert!(stats.sends > 0 && stats.recvs > 0 && stats.ticks > 0);
}

#[cfg(unix)]
#[test]
fn sealed_posix_echo() {
    run_posix_echo_sealed();
}

/// Arms an accept, connects `Sock(1)`, and arms a `Recv` on the accepted slot.
/// Returns `None` when any step fails.
fn accept_and_arm_recv(sim: &mut SimIo, out: &mut Completions, addr: SocketAddr) -> Option<Sock> {
    sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .ok()?;
    sim.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .ok()?;
    let (mut srv_opt, mut cli_ok) = (None, false);
    for _ in 0..64 {
        sim.tick(false, out).ok()?;
        while let Some(c) = out.pop() {
            if c.token == Token(1) && c.result >= 0 {
                srv_opt = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token == Token(2) && c.result == 0 {
                cli_ok = true;
            }
        }
        if srv_opt.is_some() && cli_ok {
            break;
        }
    }
    let srv = srv_opt?;
    sim.submit(Token(3), Op::ArmedRecv { sock: srv }).ok()?;
    Some(srv)
}

/// Waits for one round's send and its ring-buffer receive. `None` when a tick fails.
fn await_round(sim: &mut SimIo, out: &mut Completions, round: u32) -> Option<bool> {
    let (mut sent_ok, mut recvd_ok) = (false, false);
    for _ in 0..32 {
        sim.tick(false, out).ok()?;
        while let Some(c) = out.pop() {
            if c.token == Token(100 + u64::from(round)) && c.result == 16 {
                sent_ok = true;
            } else if c.token == Token(3) && c.result > 0 {
                if let Some(id) = c.buf {
                    if sim.buf(id)[0] == 0x5C {
                        recvd_ok = true;
                    }
                    sim.give_back(id);
                }
            }
        }
        if sent_ok && recvd_ok {
            return Some(true);
        }
    }
    Some(false)
}

/// Runs 40 send/receive rounds. Returns the completed count and whether every tick succeeded.
fn echo_rounds(sim: &mut SimIo, out: &mut Completions) -> (u32, bool) {
    let mut cycles = 0u32;
    for round in 0..40u32 {
        sim.buf(BufId(0))[..16].copy_from_slice(&[0x5Cu8; 16]);
        let send = Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 16,
            zero_copy: false,
        };
        if sim.submit(Token(100 + u64::from(round)), send).is_err() {
            break;
        }
        match await_round(sim, out, round) {
            None => return (cycles, false),
            Some(true) => cycles += 1,
            Some(false) => {}
        }
    }
    (cycles, true)
}

pub fn run_armed_net_ring_sealed() {
    let limits = Limits {
        sockets_max: 12,
        buffers: 2,
        buffer_size: 4096,
        accept_range_start: 6,
        accept_range_len: 4,
        ring_buffers: 8,
        ring_buffer_size: 1024,
        ops_max: 16,
        ..Limits::BASE
    };
    let mut sim = SimIo::new(limits, 9002, Faults::default());
    let mut out = Completions::new(16);
    let addr = sim
        .listen(Sock(0), "192.0.2.90:9002".parse().expect("addr"))
        .expect("listen");

    let completed = sealed(|| {
        let Some(srv) = accept_and_arm_recv(&mut sim, &mut out, addr) else {
            return 0;
        };
        let (cycles, ticks_ok) = echo_rounds(&mut sim, &mut out);
        if !ticks_ok {
            return cycles;
        }
        // Teardown after the count is settled: a busy ring or a failed tick does not change it.
        let _: Option<()> = sim.submit(Token(900), Op::Close { sock: srv }).ok();
        let _: Option<()> = sim.submit(Token(901), Op::Close { sock: Sock(1) }).ok();
        for _ in 0..16 {
            let _: Option<()> = sim.tick(false, &mut out).ok();
            while out.pop().is_some() {}
        }
        cycles
    });
    assert_eq!(completed, 40);
}

#[test]
fn sealed_armed_net_ring() {
    run_armed_net_ring_sealed();
}
