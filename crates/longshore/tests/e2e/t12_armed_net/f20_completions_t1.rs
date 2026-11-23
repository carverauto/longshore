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
    let c = Completion::new(Token::new(7), 42);
    assert_eq!(c.token(), Token::new(7));
    assert_eq!(c.result(), 42);
    assert!(!c.more());
    assert!(c.buf().is_none());
}

#[cfg(feature = "sim")]
#[test]
fn t1_f20_2_single_shot_ops_always_deliver_final_without_buf() {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_ops_max(8);
    let mut io = SimIo::new(limits, 2002, Faults::default());
    let mut out = Completions::new(16);

    io.submit(Token::new(1), Op::Timeout { nanos: 1000 })
        .unwrap();
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(2),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(3),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 50, |v| v.len() == 3).unwrap();
    for c in cs {
        assert!(!c.more(), "single-shot op must have more=false: {c:?}");
        assert!(
            c.buf().is_none(),
            "single-shot op must have buf=None: {c:?}"
        );
    }

    write_pool_bytes(&mut io, BufId::new(0), 0, b"single-shot");
    io.submit(
        Token::new(4),
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: 11,
            zero_copy: false,
        },
    )
    .unwrap();
    let sent = drive_until(&mut io, &mut out, 20, |v| v.len() == 1).unwrap();
    assert_eq!(sent[0], Completion::new(Token::new(4), 11));
}

#[cfg(feature = "sim")]
#[test]
fn t1_f20_3_non_armed_trace_hash_parity_preserved() {
    let limits = Limits::BASE
        .with_sockets_max(3)
        .with_buffers(3)
        .with_buffer_size(4096)
        .with_ops_max(4);
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

/// Accepts `Sock(2)` from a `Sock(1)` connect on a fresh simulator.
#[cfg(feature = "sim")]
fn sim_with_pair() -> (SimIo, Completions) {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(2)
        .with_buffer_size(1024)
        .with_ops_max(8)
        .with_ring_buffers(4)
        .with_ring_buffer_size(1024);
    let mut io = SimIo::new(limits, 2004, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();
    (io, out)
}

/// Sends `bytes` from `Sock(1)` and drives until the armed receive (token 3) completes.
#[cfg(feature = "sim")]
fn send_and_await_armed_recv(
    io: &mut SimIo,
    out: &mut Completions,
    token: Token,
    bytes: &[u8],
) -> Vec<Completion> {
    write_pool_bytes(io, BufId::new(0), 0, bytes);
    io.submit(
        token,
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: u32::try_from(bytes.len()).unwrap(),
            zero_copy: false,
        },
    )
    .unwrap();
    drive_until(io, out, 30, |v| v.iter().any(|c| c.token().get() == 3)).unwrap()
}

/// Trace hash after two armed receives, either holding or returning the first ring buffer.
#[cfg(feature = "sim")]
fn armed_recv_trace_hash(consume_one_first: bool) -> u64 {
    let (mut io, mut out) = sim_with_pair();
    io.submit(Token::new(3), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();
    let cs = send_and_await_armed_recv(&mut io, &mut out, Token::new(4), b"first");
    let r = cs.iter().find(|c| c.token().get() == 3).unwrap();
    assert!(r.more());
    let id = r.buf().expect("ring buf id");
    if consume_one_first {
        let _: BufId = id;
    } else {
        io.give_back(id);
    }
    let _: Vec<Completion> = send_and_await_armed_recv(&mut io, &mut out, Token::new(5), b"second");
    io.trace_hash()
}

#[cfg(feature = "sim")]
#[test]
fn t1_f20_4_armed_completion_more_and_buf_affect_trace_hash() {
    assert_ne!(armed_recv_trace_hash(true), armed_recv_trace_hash(false));
}

#[cfg(target_os = "linux")]
#[test]
fn t1_f20_5_uring_cq_sized_with_nodrop_handles_burst_without_drop() {
    let limits = Limits::BASE
        .with_sockets_max(16)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_ops_max(16)
        .with_accept_range_start(4)
        .with_accept_range_len(8)
        .with_ring_buffers(16)
        .with_ring_buffer_size(1024);
    let mut io = match UringIo::new(limits) {
        Ok(u) => u,
        Err(e) if e.raw_os_error() == Some(EPERM) || e.raw_os_error() == Some(ENOSYS) => {
            return;
        }
        Err(e) => panic!("UringIo::new failed: {e}"),
    };
    let mut out = Completions::new(64);
    let addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .expect("listen");
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    for i in 0..4u32 {
        io.submit(
            Token::new(10 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(12 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut io, &mut out, 100, |v| {
        v.iter().filter(|c| c.token().get() == 1).count() == 4
            && v.iter().filter(|c| c.token().get() >= 10).count() == 4
    })
    .expect("all 8 completions arrive without drop");
    assert_eq!(cs.iter().filter(|c| c.token().get() == 1).count(), 4);
}
