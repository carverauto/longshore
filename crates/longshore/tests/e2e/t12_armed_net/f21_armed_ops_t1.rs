// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 21 Tier 1 E2E tests: armed `Accept`, armed `Recv`, buffer ring & `give_back`.

#[cfg(unix)]
use super::super::harness::posix_backend;
#[cfg(any(feature = "sim", unix))]
use super::super::harness::{drive_until, read_pool_bytes, write_pool_bytes};
#[cfg(any(feature = "sim", unix))]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(feature = "sim")]
use std::collections::BTreeSet;
#[cfg(feature = "sim")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(feature = "sim")]
pub(super) const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 21)), 9100);

#[cfg(feature = "sim")]
pub(super) fn armed_limits(accept_len: u32, ring_bufs: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(16 + accept_len)
        .with_buffers(4)
        .with_buffer_size(1024)
        .with_ops_max(64)
        .with_accept_range_start(16)
        .with_accept_range_len(accept_len)
        .with_ring_buffers(ring_bufs)
        .with_ring_buffer_size(1024)
}

#[cfg(feature = "sim")]
#[test]
fn t1_f21_1_armed_accept_confines_slots_to_range_and_stays_armed() {
    let mut io = SimIo::new(armed_limits(4, 4), 2101, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();

    for i in 0..3u32 {
        io.submit(
            Token::new(10 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut io, &mut out, 80, |v| {
        v.iter().filter(|c| c.token().get() == 1).count() == 3
            && v.iter().filter(|c| c.token().get() >= 10).count() == 3
    })
    .unwrap();

    let mut slots = BTreeSet::new();
    for c in cs.iter().filter(|c| c.token().get() == 1) {
        assert!(c.more(), "accept must remain armed");
        assert!(c.buf().is_none());
        let slot = u32::try_from(c.result()).unwrap();
        assert!((16..20).contains(&slot), "slot {slot} outside 16..20");
        assert!(slots.insert(slot), "duplicate accepted slot {slot}");
    }
    assert_eq!(slots.len(), 3);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f21_2_idle_armed_recv_connections_consume_zero_ring_buffers() {
    let limits = Limits::BASE
        .with_sockets_max(64)
        .with_buffers(2)
        .with_buffer_size(512)
        .with_ops_max(64)
        .with_accept_range_start(30)
        .with_accept_range_len(24)
        .with_ring_buffers(2)
        .with_ring_buffer_size(512);
    let mut io = SimIo::new(limits, 2102, Faults::default());
    let mut out = Completions::new(64);
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();

    let mut server_socks = Vec::new();
    for i in 0..20u32 {
        io.submit(
            Token::new(100 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token().get() == 1)
                && v.iter()
                    .any(|c| c.token() == Token::new(100 + u64::from(i)))
        })
        .unwrap();
        let accepted = cs.iter().find(|c| c.token().get() == 1).unwrap();
        let s = Sock::new(u32::try_from(accepted.result()).unwrap());
        server_socks.push(s);
        io.submit(Token::new(200 + u64::from(i)), Op::ArmedRecv { sock: s })
            .unwrap();
    }

    io.tick(false, &mut out).unwrap();
    assert!(out.is_empty());

    write_pool_bytes(&mut io, BufId::new(0), 0, b"ring-still-free");
    io.submit(
        Token::new(500),
        Op::Send {
            sock: Sock::new(20),
            buf: BufId::new(0),
            off: 0,
            len: 15,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token().get() == 219)
    })
    .unwrap();
    let got = cs.iter().find(|c| c.token().get() == 219).unwrap();
    assert_eq!(got.result(), 15);
    assert!(got.more());
    let id = got.buf().unwrap();
    assert_eq!(read_pool_bytes(&mut io, id, 0, 15), b"ring-still-free");
    io.give_back(id);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f21_3_armed_recv_reads_in_place_across_five_sockets() {
    let mut io = SimIo::new(armed_limits(8, 8), 2103, Faults::default());
    let mut out = Completions::new(32);
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();

    let mut pairs = Vec::new();
    for i in 0..5u32 {
        io.submit(
            Token::new(10 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token().get() == 1)
                && v.iter().any(|c| c.token() == Token::new(10 + u64::from(i)))
        })
        .unwrap();
        let s = Sock::new(
            u32::try_from(cs.iter().find(|c| c.token().get() == 1).unwrap().result()).unwrap(),
        );
        io.submit(Token::new(50 + u64::from(i)), Op::ArmedRecv { sock: s })
            .unwrap();
        pairs.push((Sock::new(1 + i), s));
    }

    for (idx, (c_sock, _)) in pairs.iter().enumerate() {
        let msg = format!("msg-from-client-{idx}");
        write_pool_bytes(&mut io, BufId::new(0), 0, msg.as_bytes());
        io.submit(
            Token::new(100 + idx as u64),
            Op::Send {
                sock: *c_sock,
                buf: BufId::new(0),
                off: 0,
                len: msg.len() as u32,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token() == Token::new(50 + idx as u64))
        })
        .unwrap();
        let rc = cs
            .iter()
            .find(|c| c.token() == Token::new(50 + idx as u64))
            .unwrap();
        assert!(rc.more());
        let id = rc.buf().expect("buf id");
        assert_eq!(
            read_pool_bytes(&mut io, id, 0, usize::try_from(rc.result()).unwrap()),
            msg.as_bytes()
        );
        io.give_back(id);
    }
}

#[cfg(unix)]
#[test]
fn t1_f21_4_give_back_recycles_ring_buffer_ids_without_leak() {
    let limits = Limits::BASE
        .with_sockets_max(8)
        .with_buffers(2)
        .with_buffer_size(1024)
        .with_ops_max(16)
        .with_accept_range_start(4)
        .with_accept_range_len(4)
        .with_ring_buffers(2)
        .with_ring_buffer_size(1024);
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(16);
    let addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
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
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 50, |v| v.len() == 2).unwrap();
    io.submit(Token::new(3), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();

    let mut seen_ids = Vec::new();
    for round in 0..5u8 {
        let msg = [b'A' + round; 8];
        write_pool_bytes(&mut io, BufId::new(0), 0, &msg);
        io.submit(
            Token::new(10 + u64::from(round)),
            Op::Send {
                sock: Sock::new(1),
                buf: BufId::new(0),
                off: 0,
                len: 8,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 50, |v| {
            v.iter().any(|c| c.token().get() == 3)
        })
        .unwrap();
        let rc = cs.iter().find(|c| c.token().get() == 3).unwrap();
        assert!(rc.more());
        let id = rc.buf().expect("buf");
        assert_eq!(read_pool_bytes(&mut io, id, 0, 8), msg);
        seen_ids.push(id);
        io.give_back(id);
    }
    let unique: std::collections::BTreeSet<u32> = seen_ids.iter().map(|b| b.get()).collect();
    assert!(unique.len() <= 2);
    assert_eq!(seen_ids.len(), 5);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f21_5_peer_close_ends_armed_recv_with_zero_and_none() {
    let mut io = SimIo::new(armed_limits(4, 2), 2105, Faults::default());
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
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

    io.submit(Token::new(3), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();
    io.submit(Token::new(4), Op::Close { sock: Sock::new(1) })
        .unwrap();
    let cs = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token().get() == 3) && v.iter().any(|c| c.token().get() == 4)
    })
    .unwrap();
    let eof = cs.iter().find(|c| c.token().get() == 3).unwrap();
    assert_eq!(eof.result(), 0);
    assert!(!eof.more());
    assert!(eof.buf().is_none());
}
