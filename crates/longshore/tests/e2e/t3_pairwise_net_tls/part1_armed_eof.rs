// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-11`–`T3-12`: `Posix` armed `Accept`/`Recv`
//! with `Completions::new(1)` + ring recycling, and close cancelling armed `Recv`
//! with shutdown-before-close peer EOF.

use super::super::harness::{drive_until, read_pool_bytes, submit_and_wait_one, write_pool_bytes};
use super::net_limits;
use longshore::io::{BufId, Completions, Io, Op, Sock, Token};

#[cfg(unix)]
#[test]
fn t3_11_posix_armed_accept_recv_small_completions_and_ring_recycle() {
    use super::super::harness::posix_backend;

    let limits = net_limits(16, 8, 6, 4, 1024);
    let mut io = posix_backend(limits).expect("posix");
    let addr = io
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .expect("listen");
    let mut tiny_out = Completions::new(1);

    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    for i in 0..4u32 {
        io.submit(
            Token(10 + u64::from(i)),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
    }

    let mut accepted = Vec::new();
    let mut connected = 0usize;
    let cs = drive_until(&mut io, &mut tiny_out, 10_000, |v| {
        connected = v.iter().filter(|c| (10..14).contains(&c.token.0)).count();
        accepted.clear();
        for c in v.iter().filter(|c| c.token == Token(1)) {
            assert!(c.more, "armed accept must stay armed with more=true");
            assert!(c.result >= 8 && c.result < 14);
            accepted.push(Sock(u32::try_from(c.result).unwrap()));
        }
        connected == 4 && accepted.len() == 4
    })
    .unwrap();
    assert!(cs
        .iter()
        .filter(|c| (10..14).contains(&c.token.0))
        .all(|c| c.result >= 0));

    for (idx, &srv) in accepted.iter().enumerate() {
        io.submit(Token(100 + idx as u64), Op::ArmedRecv { sock: srv })
            .unwrap();
        let msg = format!("msg-{idx}");
        write_pool_bytes(&mut io, BufId(0), idx * 16, msg.as_bytes());
        io.submit(
            Token(200 + idx as u64),
            Op::Send {
                sock: Sock(1 + idx as u32),
                buf: BufId(0),
                off: (idx * 16) as u32,
                len: msg.len() as u32,
                zero_copy: false,
            },
        )
        .unwrap();
    }

    let mut seen_ids = Vec::new();
    let recv_cs = drive_until(&mut io, &mut tiny_out, 10_000, |v| {
        v.iter().filter(|c| (100..104).contains(&c.token.0)).count() == 4
    })
    .unwrap();
    for c in recv_cs.iter().filter(|c| (100..104).contains(&c.token.0)) {
        assert!(c.more);
        let id = c.buf.expect("armed recv completion must name ring buffer");
        seen_ids.push(id);
        let idx = (c.token.0 - 100) as usize;
        let expected = format!("msg-{idx}");
        assert_eq!(
            read_pool_bytes(&mut io, id, 0, usize::try_from(c.result).unwrap()),
            expected.as_bytes()
        );
        io.give_back(id);
    }
    assert_eq!(seen_ids.len(), 4);

    for idx in 0..4usize {
        write_pool_bytes(&mut io, BufId(0), 0, b"wave2");
        io.submit(
            Token(300 + idx as u64),
            Op::Send {
                sock: Sock(1 + idx as u32),
                buf: BufId(0),
                off: 0,
                len: 5,
                zero_copy: false,
            },
        )
        .unwrap();
    }
    let wave2 = drive_until(&mut io, &mut tiny_out, 10_000, |v| {
        v.iter().filter(|c| (100..104).contains(&c.token.0)).count() == 4
    })
    .unwrap();
    for c in wave2.iter().filter(|c| (100..104).contains(&c.token.0)) {
        let id = c.buf.expect("recycled ring buf");
        assert!(seen_ids.contains(&id), "ring buffer should recycle");
        assert_eq!(
            read_pool_bytes(&mut io, id, 0, usize::try_from(c.result).unwrap()),
            b"wave2"
        );
        io.give_back(id);
    }
}

fn verify_shutdown_before_close<IO: Io>(io: &mut IO, addr: std::net::SocketAddr) {
    let mut out = Completions::new(16);
    let bound = io.listen(Sock(0), addr).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    let srv = Sock(u32::try_from(cs.iter().find(|c| c.token == Token(1)).unwrap().result).unwrap());
    assert_eq!(cs.iter().find(|c| c.token == Token(2)).unwrap().result, 0);

    io.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();
    write_pool_bytes(io, BufId(0), 0, b"hold");
    io.submit(
        Token(4),
        Op::Send {
            sock: srv,
            buf: BufId(0),
            off: 0,
            len: 4,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(Token(5), Op::Close { sock: srv }).unwrap();
    let close_cs = drive_until(io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more)
            && v.iter().any(|c| c.token == Token(5) && !c.more)
    })
    .unwrap();
    assert!(!close_cs.iter().find(|c| c.token == Token(3)).unwrap().more);
    assert_eq!(
        close_cs
            .iter()
            .find(|c| c.token == Token(5))
            .unwrap()
            .result,
        0
    );

    loop {
        let r = submit_and_wait_one(
            io,
            &mut out,
            Token(6),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 64,
            },
            10_000,
        )
        .unwrap();
        assert!(!r.more && r.buf.is_none());
        if r.result == 0 {
            break;
        }
        assert!(r.result > 0);
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
