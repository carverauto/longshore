// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 20 Tier 1 E2E tests: `Completion` fields, trace parity & `NODROP`.

#[cfg(any(feature = "sim", target_os = "linux"))]
use super::super::harness::drive_until;
#[cfg(feature = "sim")]
use super::super::harness::write_pool_bytes;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(feature = "sim")]
use longshore::io::BufId;
use longshore::io::{Completion, Token};
#[cfg(any(feature = "sim", target_os = "linux"))]
use longshore::io::{Completions, Io, Limits, Op, Sock};
#[cfg(feature = "sim")]
use longshore::sim::{scenario, Faults, SimIo};
#[cfg(feature = "sim")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(feature = "sim")]
pub(super) const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)), 9000);
#[cfg(target_os = "linux")]
const EPERM: i32 = 1;
#[cfg(target_os = "linux")]
const ENOSYS: i32 = 38;

#[test]
fn t1_f20_1_completion_new_clears_more_and_buf() {
    let c = Completion::new(Token(7), 42);
    assert_eq!(
        c,
        Completion {
            token: Token(7),
            result: 42,
            more: false,
            buf: None,
        }
    );
    assert!(!c.more);
    assert!(c.buf.is_none());
}

#[cfg(feature = "sim")]
#[test]
fn t1_f20_2_single_shot_ops_always_deliver_final_without_buf() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 2002, Faults::default());
    let mut out = Completions::new(16);

    io.submit(Token(1), Op::Timeout { nanos: 1000 }).unwrap();
    let addr = io.listen(Sock(0), ADDR).unwrap();
    io.submit(
        Token(2),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(3),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 50, |v| v.len() == 3).unwrap();
    for c in cs {
        assert!(!c.more, "single-shot op must have more=false: {c:?}");
        assert!(c.buf.is_none(), "single-shot op must have buf=None: {c:?}");
    }

    write_pool_bytes(&mut io, BufId(0), 0, b"single-shot");
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 11,
            zero_copy: false,
        },
    )
    .unwrap();
    let sent = drive_until(&mut io, &mut out, 20, |v| v.len() == 1).unwrap();
    assert_eq!(sent[0], Completion::new(Token(4), 11));
}

#[cfg(feature = "sim")]
#[test]
fn t1_f20_3_non_armed_trace_hash_parity_preserved() {
    let limits = Limits {
        sockets_max: 3,
        buffers: 3,
        buffer_size: 4096,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut hashes = Vec::new();
    for seed in 0..5u64 {
        let mut a = SimIo::new(limits, seed, Faults::default());
        let mut out_a = Completions::new(16);
        let _: longshore::io::scenario::EchoStats =
            scenario::echo(&mut a, ADDR, 8192, 1024, false, &mut out_a).expect("echo a");

        let mut b = SimIo::new(limits, seed, Faults::default());
        let mut out_b = Completions::new(16);
        let _: longshore::io::scenario::EchoStats =
            scenario::echo(&mut b, ADDR, 8192, 1024, false, &mut out_b).expect("echo b");

        assert_eq!(a.trace_hash(), b.trace_hash());
        hashes.push(a.trace_hash());
    }
    assert_ne!(hashes[0], 0);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f20_4_armed_completion_more_and_buf_affect_trace_hash() {
    let run_with_skip = |consume_one_first: bool| -> u64 {
        let limits = Limits {
            sockets_max: 4,
            buffers: 2,
            buffer_size: 1024,
            ops_max: 8,
            ring_buffers: 4,
            ring_buffer_size: 1024,
            ..Limits::BASE
        };
        let mut io = SimIo::new(limits, 2004, Faults::default());
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
        let _: Vec<Completion> = drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

        io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
            .unwrap();
        write_pool_bytes(&mut io, BufId(0), 0, b"first");
        io.submit(
            Token(4),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 5,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token == Token(3))
        })
        .unwrap();
        let r = cs.iter().find(|c| c.token == Token(3)).unwrap();
        assert!(r.more);
        let id = r.buf.expect("ring buf id");
        if consume_one_first {
            let _: BufId = id;
        } else {
            io.give_back(id);
        }
        write_pool_bytes(&mut io, BufId(0), 0, b"second");
        io.submit(
            Token(5),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 6,
                zero_copy: false,
            },
        )
        .unwrap();
        let _: Vec<Completion> = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token == Token(3))
        })
        .unwrap();
        io.trace_hash()
    };
    assert_ne!(run_with_skip(true), run_with_skip(false));
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f20_5_uring_cq_sized_with_nodrop_handles_burst_without_drop() {
    let limits = Limits {
        sockets_max: 16,
        buffers: 4,
        buffer_size: 4096,
        ops_max: 16,
        accept_range_start: 4,
        accept_range_len: 8,
        ring_buffers: 16,
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
    let mut out = Completions::new(64);
    let addr = io
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .expect("listen");
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for i in 0..4u32 {
        io.submit(
            Token(10 + u64::from(i)),
            Op::Connect {
                sock: Sock(12 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut io, &mut out, 100, |v| {
        v.iter().filter(|c| c.token == Token(1)).count() == 4
            && v.iter().filter(|c| c.token.0 >= 10).count() == 4
    })
    .expect("all 8 completions arrive without drop");
    assert_eq!(cs.iter().filter(|c| c.token == Token(1)).count(), 4);
}
