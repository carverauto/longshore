// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 23 Tier 1 (part B) E2E tests: reset faults & Linux kernel server checks.

#[cfg(any(feature = "sim", target_os = "linux"))]
use super::super::harness::drive_until;
#[cfg(target_os = "linux")]
use super::super::harness::write_pool_bytes as write_pool_bytes_linux;
#[cfg(feature = "sim")]
use super::super::harness::{read_pool_bytes, write_pool_bytes};
#[cfg(feature = "sim")]
use super::f23_server_shared_t1a::server_limits;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(target_os = "linux")]
use longshore::io::Limits;
#[cfg(any(feature = "sim", target_os = "linux"))]
use longshore::io::{BufId, Completions, Io, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

#[cfg(target_os = "linux")]
const EPERM: i32 = 1;
#[cfg(target_os = "linux")]
const ENOSYS: i32 = 38;

#[cfg(feature = "sim")]
#[test]
fn t1_f23_3_sim_armed_recv_under_reset_delivers_prior_bytes_then_final_reset() {
    let faults = Faults {
        reset: 100_000,
        ..Default::default()
    };
    let mut io = SimIo::new(server_limits(), 2303, faults);
    let mut out = Completions::new(32);
    let addr = io
        .listen(Sock(0), "192.0.2.23:9303".parse().unwrap())
        .unwrap();
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    drive_until(&mut io, &mut out, 60, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let mut sent = 0u64;
    let mut recvd = Vec::new();
    let mut final_res = None;
    for round in 0..200u64 {
        let byte = (round & 0xff) as u8;
        write_pool_bytes(&mut io, BufId(0), 0, &[byte; 32]);
        if io
            .submit(
                Token(10 + round),
                Op::Send {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: 32,
                    zero_copy: false,
                },
            )
            .is_err()
        {
            break;
        }
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(3) {
                if let Some(id) = c.buf {
                    let chunk = read_pool_bytes(&mut io, id, 0, c.result as usize);
                    for (idx, b) in chunk.iter().enumerate() {
                        assert_eq!(*b, (((recvd.len() + idx) / 32) & 0xff) as u8);
                    }
                    recvd.extend_from_slice(&chunk);
                    io.give_back(id);
                }
                if !c.more {
                    final_res = Some(c.result);
                }
            } else if c.token.0 >= 10 && c.result > 0 {
                sent += c.result as u64;
            }
        }
        if final_res.is_some() {
            break;
        }
    }
    assert!(sent > 0 || final_res.is_some());
    assert!(matches!(final_res, Some(r) if r <= 0));
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f23_4_uring_many_idle_connections_hold_zero_ring_buffers() {
    let limits = Limits {
        sockets_max: 256,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 256,
        accept_range_start: 128,
        accept_range_len: 128,
        ring_buffers: 4,
        ring_buffer_size: 1024,
        ..Limits::BASE
    };
    let mut io = match UringIo::new(limits) {
        Ok(u) => u,
        Err(e) if e.raw_os_error() == Some(EPERM) || e.raw_os_error() == Some(ENOSYS) => {
            return;
        }
        Err(e) => panic!("UringIo::new: {e}"),
    };
    let mut out = Completions::new(64);
    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for i in 0..64u32 {
        io.submit(
            Token(100 + i as u64),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 50, |v| {
            v.iter().any(|c| c.token == Token(1))
                && v.iter().any(|c| c.token == Token(100 + i as u64))
        })
        .unwrap();
        let srv = Sock(cs.iter().find(|c| c.token == Token(1)).unwrap().result as u32);
        io.submit(Token(500 + i as u64), Op::ArmedRecv { sock: srv })
            .unwrap();
    }
    write_pool_bytes_linux(&mut io, BufId(0), 0, b"still-has-ring");
    io.submit(
        Token(999),
        Op::Send {
            sock: Sock(64),
            buf: BufId(0),
            off: 0,
            len: 14,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(563))
    })
    .unwrap();
    let c = cs.iter().find(|c| c.token == Token(563)).unwrap();
    assert_eq!(c.result, 14);
    io.give_back(c.buf.unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f23_5_uring_cq_burst_delivers_all_armed_completions() {
    let limits = Limits {
        sockets_max: 32,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 32,
        accept_range_start: 16,
        accept_range_len: 16,
        ring_buffers: 16,
        ring_buffer_size: 1024,
        ..Limits::BASE
    };
    let mut io = match UringIo::new(limits) {
        Ok(u) => u,
        Err(e) if e.raw_os_error() == Some(EPERM) || e.raw_os_error() == Some(ENOSYS) => {
            return;
        }
        Err(e) => panic!("UringIo::new: {e}"),
    };
    let mut out = Completions::new(64);
    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for i in 0..8u32 {
        io.submit(
            Token(10 + i as u64),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut io, &mut out, 80, |v| {
        v.iter().filter(|c| c.token == Token(1)).count() == 8
    })
    .unwrap();
    assert_eq!(cs.iter().filter(|c| c.token == Token(1)).count(), 8);
}
