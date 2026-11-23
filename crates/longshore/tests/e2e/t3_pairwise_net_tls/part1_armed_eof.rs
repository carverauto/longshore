// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-11`–`T3-12`: `Posix` armed `Accept`/`Recv`
//! with `Completions::new(1)` + ring recycling, and close cancelling armed `Recv`
//! with shutdown-before-close peer EOF.

use super::super::harness::{drive_until, read_pool_bytes, submit_and_wait_one, write_pool_bytes};
use super::net_limits;
use longshore::io::{BufId, Completion, Completions, Io, Op, Sock, Token};

/// Connects four clients to an armed accept on `Sock(0)`; returns the
/// accepted server sockets once all four sides have completed.
#[cfg(unix)]
fn accept_four_armed<IO: Io>(
    io: &mut IO,
    tiny_out: &mut Completions,
    addr: std::net::SocketAddr,
) -> Vec<Sock> {
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
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
    }

    let mut accepted = Vec::new();
    let mut connected = 0usize;
    let cs = drive_until(io, tiny_out, 10_000, |v| {
        connected = v
            .iter()
            .filter(|c| (10..14).contains(&c.token().get()))
            .count();
        accepted.clear();
        for c in v.iter().filter(|c| c.token().get() == 1) {
            assert!(c.more(), "armed accept must stay armed with more=true");
            assert!(c.result() >= 8 && c.result() < 14);
            accepted.push(Sock::new(u32::try_from(c.result()).unwrap()));
        }
        connected == 4 && accepted.len() == 4
    })
    .unwrap();
    assert!(cs
        .iter()
        .filter(|c| (10..14).contains(&c.token().get()))
        .all(|c| c.result() >= 0));
    accepted
}

/// Arms a recv on each accepted socket and sends `msg-{idx}` from each client.
#[cfg(unix)]
fn arm_and_send_greetings<IO: Io>(io: &mut IO, accepted: &[Sock]) {
    for (idx, &srv) in accepted.iter().enumerate() {
        io.submit(Token::new(100 + idx as u64), Op::ArmedRecv { sock: srv })
            .unwrap();
        let msg = format!("msg-{idx}");
        write_pool_bytes(io, BufId::new(0), idx * 16, msg.as_bytes());
        io.submit(
            Token::new(200 + idx as u64),
            Op::Send {
                sock: Sock::new(1 + idx as u32),
                buf: BufId::new(0),
                off: (idx * 16) as u32,
                len: msg.len() as u32,
                zero_copy: false,
            },
        )
        .unwrap();
    }
}

/// Receives the four greetings through ring buffers, checks each, gives the
/// buffers back, and returns the ring buffer ids seen.
#[cfg(unix)]
fn recv_greetings<IO: Io>(io: &mut IO, tiny_out: &mut Completions) -> Vec<BufId> {
    let mut seen_ids = Vec::new();
    let recv_cs = drive_until(io, tiny_out, 10_000, |v| {
        v.iter()
            .filter(|c| (100..104).contains(&c.token().get()))
            .count()
            == 4
    })
    .unwrap();
    for c in recv_cs
        .iter()
        .filter(|c| (100..104).contains(&c.token().get()))
    {
        assert!(c.more());
        let id = c
            .buf()
            .expect("armed recv completion must name ring buffer");
        seen_ids.push(id);
        let idx = (c.token().get() - 100) as usize;
        let expected = format!("msg-{idx}");
        assert_eq!(
            read_pool_bytes(io, id, 0, usize::try_from(c.result()).unwrap()),
            expected.as_bytes()
        );
        io.give_back(id);
    }
    assert_eq!(seen_ids.len(), 4);
    seen_ids
}

/// Sends a second wave and checks it lands in the recycled ring buffers.
#[cfg(unix)]
fn second_wave_recycles<IO: Io>(io: &mut IO, tiny_out: &mut Completions, seen_ids: &[BufId]) {
    for idx in 0..4usize {
        write_pool_bytes(io, BufId::new(0), 0, b"wave2");
        io.submit(
            Token::new(300 + idx as u64),
            Op::Send {
                sock: Sock::new(1 + idx as u32),
                buf: BufId::new(0),
                off: 0,
                len: 5,
                zero_copy: false,
            },
        )
        .unwrap();
    }
    let wave2 = drive_until(io, tiny_out, 10_000, |v| {
        v.iter()
            .filter(|c| (100..104).contains(&c.token().get()))
            .count()
            == 4
    })
    .unwrap();
    for c in wave2
        .iter()
        .filter(|c| (100..104).contains(&c.token().get()))
    {
        let id = c.buf().expect("recycled ring buf");
        assert!(seen_ids.contains(&id), "ring buffer should recycle");
        assert_eq!(
            read_pool_bytes(io, id, 0, usize::try_from(c.result()).unwrap()),
            b"wave2"
        );
        io.give_back(id);
    }
}

#[cfg(unix)]
#[test]
fn t3_11_posix_armed_accept_recv_small_completions_and_ring_recycle() {
    use super::super::harness::posix_backend;

    let limits = net_limits(16, 8, 6, 4, 1024);
    let mut io = posix_backend(limits).expect("posix");
    let addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .expect("listen");
    let mut tiny_out = Completions::new(1);

    let accepted = accept_four_armed(&mut io, &mut tiny_out, addr);
    arm_and_send_greetings(&mut io, &accepted);
    let seen_ids = recv_greetings(&mut io, &mut tiny_out);
    second_wave_recycles(&mut io, &mut tiny_out, &seen_ids);
}

/// The completion for token `t` among `cs`.
fn completion_of(cs: &[Completion], t: u64) -> Completion {
    *cs.iter().find(|c| c.token().get() == t).unwrap()
}

fn verify_shutdown_before_close<IO: Io>(io: &mut IO, addr: std::net::SocketAddr) {
    let mut out = Completions::new(16);
    let bound = io.listen(Sock::new(0), addr).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, &mut out, 10_000, |v| {
        [1, 2]
            .iter()
            .all(|&t| v.iter().any(|c| c.token().get() == t))
    })
    .unwrap();
    let srv = Sock::new(u32::try_from(completion_of(&cs, 1).result()).unwrap());
    assert_eq!(completion_of(&cs, 2).result(), 0);

    io.submit(Token::new(3), Op::ArmedRecv { sock: srv })
        .unwrap();
    write_pool_bytes(io, BufId::new(0), 0, b"hold");
    io.submit(
        Token::new(4),
        Op::Send {
            sock: srv,
            buf: BufId::new(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(Token::new(5), Op::Close { sock: srv }).unwrap();
    let close_cs = drive_until(io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 3 && !c.more())
            && v.iter().any(|c| c.token().get() == 5 && !c.more())
    })
    .unwrap();
    assert!(!completion_of(&close_cs, 3).more());
    assert_eq!(completion_of(&close_cs, 5).result(), 0);

    loop {
        let r = submit_and_wait_one(
            io,
            &mut out,
            Token::new(6),
            Op::Recv {
                sock: Sock::new(1),
                buf: BufId::new(1),
                off: 0,
                len: 64,
            },
            10_000,
        )
        .unwrap();
        assert!(!r.more() && r.buf().is_none());
        if r.result() == 0 {
            break;
        }
        assert!(r.result() > 0);
    }
}

#[test]
fn t3_12_close_cancels_armed_recv_and_shutdown_gives_peer_eof() {
    #[cfg(feature = "sim")]
    {
        use longshore::sim::{Faults, SimIo};

        let mut sim = SimIo::new(net_limits(12, 6, 4, 4, 1024), 312, Faults::default());
        verify_shutdown_before_close(&mut sim, "192.0.2.31:9312".parse().unwrap());
    }
    #[cfg(unix)]
    {
        use super::super::harness::posix_backend;

        let mut posix = posix_backend(net_limits(12, 6, 4, 4, 1024)).unwrap();
        verify_shutdown_before_close(&mut posix, "127.0.0.1:0".parse().unwrap());
    }
    #[cfg(target_os = "linux")]
    {
        let mut uring = longshore::io::uring::UringIo::new(net_limits(12, 6, 4, 4, 1024)).unwrap();
        verify_shutdown_before_close(&mut uring, "127.0.0.1:0".parse().unwrap());
    }
}
