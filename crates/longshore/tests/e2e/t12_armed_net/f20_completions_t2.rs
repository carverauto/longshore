// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 20 Tier 2 E2E tests: overflow draining, armed cancel, final bytes & `ops_max`.

#[cfg(any(feature = "sim", unix))]
use super::super::harness::drive_until;
#[cfg(unix)]
use super::super::harness::posix_backend;
#[cfg(feature = "sim")]
use super::super::harness::{read_pool_bytes, write_pool_bytes};
#[cfg(feature = "sim")]
use super::f20_completions_t1::ADDR;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(feature = "sim")]
use longshore::io::BufId;
#[cfg(feature = "sim")]
use longshore::io::Busy;
#[cfg(unix)]
use longshore::io::ECANCELED;
#[cfg(any(feature = "sim", unix))]
use longshore::io::{Completions, Io, Limits, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

#[cfg(target_os = "linux")]
const EPERM: i32 = 1;
#[cfg(target_os = "linux")]
const ENOSYS: i32 = 38;

#[cfg(feature = "sim")]
#[test]
fn t2_f20_1_small_completions_buffer_drains_multi_completion_burst() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ring_buffers: 16,
        ring_buffer_size: 256,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 2011, Faults::default());
    let mut out = Completions::new(2);
    let addr = io.listen(Sock(0), ADDR).unwrap();
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
    drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let mut recvd = 0usize;
    for i in 0..10u8 {
        write_pool_bytes(&mut io, BufId(0), 0, &[i; 16]);
        io.submit(
            Token(100 + i as u64),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 16,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token == Token(3))
                && v.iter().any(|c| c.token == Token(100 + i as u64))
        })
        .unwrap();
        for c in cs {
            if c.token == Token(3) {
                assert!(c.more);
                assert_eq!(c.result, 16);
                let id = c.buf.expect("buf");
                assert_eq!(read_pool_bytes(&mut io, id, 0, 16), vec![i; 16]);
                io.give_back(id);
                recvd += 1;
            }
        }
    }
    assert_eq!(recvd, 10);
}

#[cfg(unix)]
#[test]
fn t2_f20_2_armed_recv_cancelled_delivers_single_final_completion() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ring_buffers: 4,
        ring_buffer_size: 1024,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
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
    drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());

    io.submit(Token(4), Op::Cancel { target: Token(3) })
        .unwrap();
    let cs = drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(3)) && v.iter().any(|c| c.token == Token(4))
    })
    .unwrap();
    let r = cs.iter().find(|c| c.token == Token(3)).unwrap();
    assert!(!r.more, "cancelled armed recv must set more=false");
    assert_eq!(r.result, -ECANCELED);
    assert!(r.buf.is_none());

    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty(), "no extra completions after cancel");
}

#[cfg(feature = "sim")]
#[test]
fn t2_f20_3_final_armed_recv_with_bytes_delivers_valid_buffer_payload() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ring_buffers: 2,
        ring_buffer_size: 1024,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 2013, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock(0), ADDR).unwrap();
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
    drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let payload = b"last-message-before-eof";
    write_pool_bytes(&mut io, BufId(0), 0, payload);
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: payload.len() as u32,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token == Token(3) && c.result > 0)
    })
    .unwrap();
    let c = cs.iter().find(|c| c.token == Token(3)).unwrap();
    assert_eq!(c.result, payload.len() as i32);
    let id = c.buf.expect("buffer attached");
    assert_eq!(read_pool_bytes(&mut io, id, 0, payload.len()), payload);
    io.give_back(id);
}

#[cfg(target_os = "linux")]
#[test]
fn t2_f20_4_uring_empty_armed_recv_completions_never_leak_ring_buffers() {
    let limits = Limits {
        sockets_max: 8,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ring_buffers: 2,
        ring_buffer_size: 1024,
        ..Limits::BASE
    };
    let mut io = match UringIo::new(limits) {
        Ok(u) => u,
        Err(e) if e.raw_os_error() == Some(EPERM) || e.raw_os_error() == Some(ENOSYS) => {
            return;
        }
        Err(e) => panic!("UringIo::new failed: {e}"),
    };
    let mut out = Completions::new(16);
    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
    for round in 0..6u64 {
        io.submit(
            Token(1 + round * 10),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
        io.submit(
            Token(2 + round * 10),
            Op::Connect {
                sock: Sock(1),
                addr,
            },
        )
        .unwrap();
        drive_until(&mut io, &mut out, 50, |v| {
            v.iter().any(|c| c.token == Token(1 + round * 10))
                && v.iter().any(|c| c.token == Token(2 + round * 10))
        })
        .unwrap();
        io.submit(Token(3 + round * 10), Op::ArmedRecv { sock: Sock(2) })
            .unwrap();
        io.submit(Token(4 + round * 10), Op::Close { sock: Sock(1) })
            .unwrap();
        let cs = drive_until(&mut io, &mut out, 50, |v| {
            v.iter()
                .any(|c| c.token == Token(3 + round * 10) && !c.more)
                && v.iter().any(|c| c.token == Token(4 + round * 10))
        })
        .unwrap();
        let eof = cs
            .iter()
            .find(|c| c.token == Token(3 + round * 10))
            .unwrap();
        assert_eq!(eof.result, 0);
        assert!(eof.buf.is_none());
        io.submit(Token(5 + round * 10), Op::Close { sock: Sock(2) })
            .unwrap();
        drive_until(&mut io, &mut out, 50, |v| {
            v.iter().any(|c| c.token == Token(5 + round * 10))
        })
        .unwrap();
    }
}

#[cfg(feature = "sim")]
#[test]
fn t2_f20_5_armed_op_occupies_ops_max_until_final_completion() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 2,
        ring_buffers: 2,
        ring_buffer_size: 1024,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 2015, Faults::default());
    let mut out = Completions::new(8);
    let addr = io.listen(Sock(0), ADDR).unwrap();
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
    drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    io.submit(Token(4), Op::Timeout { nanos: 5_000_000 })
        .unwrap();
    assert_eq!(io.submit(Token(5), Op::Timeout { nanos: 0 }), Err(Busy));

    drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(4))
    })
    .unwrap();
    io.submit(Token(7), Op::Cancel { target: Token(3) })
        .unwrap();
    assert_eq!(io.submit(Token(8), Op::Timeout { nanos: 0 }), Err(Busy));
    drive_until(&mut io, &mut out, 50, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more)
    })
    .unwrap();
    io.submit(Token(9), Op::Timeout { nanos: 0 }).unwrap();
}
