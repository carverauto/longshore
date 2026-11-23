// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `alloc_guard`-sealed suites for `Posix` echo and armed networking buffer rings.

use longshore::io::alloc_guard::sealed;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};

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
        if sim
            .submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .is_err()
        {
            return 0u32;
        }
        if sim
            .submit(
                Token(2),
                Op::Connect {
                    sock: Sock(1),
                    addr,
                },
            )
            .is_err()
        {
            return 0u32;
        }
        let (mut srv_opt, mut cli_ok) = (None, false);
        for _ in 0..64 {
            if sim.tick(false, &mut out).is_err() {
                return 0;
            }
            while let Some(c) = out.pop() {
                if c.token == Token(1) && c.result >= 0 {
                    srv_opt = Some(Sock(c.result as u32));
                } else if c.token == Token(2) && c.result == 0 {
                    cli_ok = true;
                }
            }
            if srv_opt.is_some() && cli_ok {
                break;
            }
        }
        let Some(srv) = srv_opt else {
            return 0;
        };
        if sim.submit(Token(3), Op::ArmedRecv { sock: srv }).is_err() {
            return 0;
        }

        let mut cycles = 0u32;
        for round in 0..40u32 {
            sim.buf(BufId(0))[..16].copy_from_slice(&[0x5Cu8; 16]);
            if sim
                .submit(
                    Token(100 + round as u64),
                    Op::Send {
                        sock: Sock(1),
                        buf: BufId(0),
                        off: 0,
                        len: 16,
                        zero_copy: false,
                    },
                )
                .is_err()
            {
                break;
            }
            let (mut sent_ok, mut recvd_ok) = (false, false);
            for _ in 0..32 {
                if sim.tick(false, &mut out).is_err() {
                    return cycles;
                }
                while let Some(c) = out.pop() {
                    if c.token == Token(100 + round as u64) && c.result == 16 {
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
                    cycles += 1;
                    break;
                }
            }
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
