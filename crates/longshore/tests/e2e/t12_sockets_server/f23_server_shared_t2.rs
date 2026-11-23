// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 23 Tier 2 E2E tests: armed faults, `alloc_guard`, peer EOF & CI wiring.

#[cfg(any(feature = "sim", target_os = "linux"))]
use super::super::harness::drive_until;
#[cfg(feature = "sim")]
use super::super::harness::{read_pool_bytes, write_pool_bytes};
#[cfg(any(feature = "sim", target_os = "linux"))]
use super::f23_server_shared_t1a::server_limits;
#[cfg(feature = "sim")]
use longshore::io::alloc_guard::sealed;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
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
fn t2_f23_1_sim_armed_accept_under_connect_refused_produces_only_valid_slots() {
    let faults = Faults {
        connect_refused: 500_000,
        ..Default::default()
    };
    let mut io = SimIo::new(server_limits(), 2311, faults);
    let mut out = Completions::new(32);
    let addr = io
        .listen(Sock(0), "192.0.2.23:9311".parse().unwrap())
        .unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    let mut accepted = 0usize;
    let mut refused = 0usize;
    for i in 0..8u32 {
        io.submit(
            Token(10 + i as u64),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token == Token(10 + i as u64))
        })
        .unwrap();
        let cc = cs.iter().find(|c| c.token == Token(10 + i as u64)).unwrap();
        if cc.result < 0 {
            refused += 1;
        } else {
            accepted += 1;
        }
        for ac in cs.iter().filter(|c| c.token == Token(1)) {
            assert!((12..20).contains(&(ac.result as u32)));
        }
    }
    assert!(accepted > 0 && refused > 0);
}

#[cfg(feature = "sim")]
#[test]
fn t2_f23_2_sim_armed_recv_observes_bit_flip_in_ring_buffer() {
    let faults = Faults {
        bit_flip: 500_000,
        ..Default::default()
    };
    let mut io = SimIo::new(server_limits(), 2312, faults);
    let mut out = Completions::new(32);
    let addr = io
        .listen(Sock(0), "192.0.2.23:9312".parse().unwrap())
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
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();
    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();

    let expected = [0xa5u8; 64];
    let mut flipped_seen = false;
    for i in 0..20u64 {
        write_pool_bytes(&mut io, BufId(0), 0, &expected);
        io.submit(
            Token(10 + i),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 64,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token == Token(3))
        })
        .unwrap();
        let rc = cs.iter().find(|c| c.token == Token(3)).unwrap();
        let id = rc.buf.unwrap();
        let got = read_pool_bytes(&mut io, id, 0, rc.result as usize);
        if got != expected[..got.len()] {
            flipped_seen = true;
        }
        io.give_back(id);
    }
    assert!(
        flipped_seen,
        "expected at least one bit_flip in ring buffer"
    );
}

#[cfg(feature = "sim")]
#[test]
fn t2_f23_3_sim_armed_accept_recv_give_back_close_runs_under_alloc_guard() {
    let mut io = SimIo::new(server_limits(), 2313, Faults::default());
    let mut out = Completions::new(32);
    let addr = io
        .listen(Sock(0), "192.0.2.23:9313".parse().unwrap())
        .unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    let completed = sealed(|| {
        let mut count = 0u32;
        for round in 0..20u64 {
            io.submit(
                Token(10 + round),
                Op::Connect {
                    sock: Sock(1),
                    addr,
                },
            )
            .unwrap();
            let mut srv_sock = None;
            let mut connected = false;
            while srv_sock.is_none() || !connected {
                io.tick(true, &mut out).unwrap();
                while let Some(c) = out.pop() {
                    if c.token == Token(1) {
                        srv_sock = Some(Sock(c.result as u32));
                    } else if c.token == Token(10 + round) {
                        connected = true;
                    }
                }
            }
            let srv = srv_sock.unwrap();
            io.submit(Token(100 + round), Op::ArmedRecv { sock: srv })
                .unwrap();
            io.buf(BufId(0))[..4].copy_from_slice(b"zero");
            io.submit(
                Token(200 + round),
                Op::Send {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: 4,
                    zero_copy: false,
                },
            )
            .unwrap();
            let mut got_buf = None;
            while got_buf.is_none() {
                io.tick(true, &mut out).unwrap();
                while let Some(c) = out.pop() {
                    if c.token == Token(100 + round) {
                        got_buf = c.buf;
                    }
                }
            }
            io.give_back(got_buf.unwrap());
            io.submit(Token(300 + round), Op::Close { sock: srv })
                .unwrap();
            io.submit(Token(400 + round), Op::Close { sock: Sock(1) })
                .unwrap();
            let mut closed = 0;
            while closed < 3 {
                io.tick(true, &mut out).unwrap();
                while let Some(c) = out.pop() {
                    if c.token.0 == 100 + round || c.token.0 >= 300 {
                        closed += 1;
                    }
                }
            }
            count += 1;
        }
        count
    });
    assert_eq!(completed, 20);
}

#[cfg(target_os = "linux")]
#[test]
fn t2_f23_4_uring_peer_eof_on_armed_close() {
    let mut io = match UringIo::new(server_limits()) {
        Ok(u) => u,
        Err(e) if e.raw_os_error() == Some(EPERM) || e.raw_os_error() == Some(ENOSYS) => {
            return;
        }
        Err(e) => panic!("UringIo::new: {e}"),
    };
    let mut out = Completions::new(16);
    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
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
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    io.submit(Token(4), Op::Close { sock: Sock(2) }).unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more) && v.iter().any(|c| c.token == Token(4))
    })
    .unwrap();
    io.submit(
        Token(5),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(5))
    })
    .unwrap();
    let eof = cs.iter().find(|c| c.token == Token(5)).unwrap();
    assert_eq!(eof.result, 0);
}

#[test]
fn t2_f23_5_ci_and_test_linux_script_reference_server_kernel_check() {
    let root = crate::e2e::harness::workspace_root();
    let script =
        std::fs::read_to_string(root.join("scripts/test-linux.sh")).expect("test-linux.sh");
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("ci.yml");
    assert!(
        script.contains("server_uring"),
        "scripts/test-linux.sh must run server_uring"
    );
    assert!(
        ci.contains("server_uring"),
        ".github/workflows/ci.yml must run server_uring"
    );
}
