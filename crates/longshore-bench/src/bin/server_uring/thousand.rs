// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! 1,000-idle-connection buffer-ring check for `server_uring`.

use super::checks::close_batch;
use longshore::io::alloc_guard::sealed;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

pub(super) fn thousand_idle_connections<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    const TOTAL: usize = 1000;
    const HALF: usize = 500;
    let srv_limits = Limits {
        sockets_max: 1002,
        accept_range_start: 1,
        accept_range_len: 1000,
        buffers: 1,
        buffer_size: 256,
        ring_buffers: 16,
        ring_buffer_size: 256,
        ops_max: 1024,
        ..Limits::BASE
    };
    let cli_limits = Limits {
        sockets_max: HALF as u32,
        buffers: 2,
        buffer_size: 256,
        ops_max: 512,
        ..Limits::BASE
    };
    let mut srv = make(srv_limits);
    let mut c0 = make(cli_limits);
    let mut c1 = make(cli_limits);
    let mut srv_out = Completions::new(128);
    let mut c_out = Completions::new(128);

    let bound = srv.listen(Sock(0), addr).expect("listen");
    let mut accepted = Vec::with_capacity(TOTAL);
    let mut client_socks = Vec::with_capacity(TOTAL);
    for k in 0..HALF {
        client_socks.push((0u8, Sock(k as u32)));
    }
    for k in 0..HALF {
        client_socks.push((1u8, Sock(k as u32)));
    }
    let mut recv_armed = [false; TOTAL];

    sealed(|| {
        srv.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .unwrap();
        let mut accept_armed = true;

        for batch in client_socks.chunks(64) {
            for (idx, &(which, sock)) in batch.iter().enumerate() {
                let op = Op::Connect { sock, addr: bound };
                if which == 0 {
                    c0.submit(Token(1000 + idx as u64), op).unwrap();
                } else {
                    c1.submit(Token(1000 + idx as u64), op).unwrap();
                }
            }
            let mut want_c = batch.len();
            let want_s = accepted.len() + batch.len();
            let mut spins = 0u32;
            while want_c > 0 || accepted.len() < want_s {
                spins += 1;
                assert!(
                    spins < 500_000,
                    "connect/accept stalled: want_c={want_c}, accepted={}/{}",
                    accepted.len(),
                    want_s
                );
                if !accept_armed && accepted.len() < want_s {
                    srv.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
                        .unwrap();
                    accept_armed = true;
                }
                c0.tick(false, &mut c_out).unwrap();
                while let Some(c) = c_out.pop() {
                    assert_eq!(c.result, 0, "c0 connect: {c:?}");
                    want_c -= 1;
                }
                c1.tick(false, &mut c_out).unwrap();
                while let Some(c) = c_out.pop() {
                    assert_eq!(c.result, 0, "c1 connect: {c:?}");
                    want_c -= 1;
                }
                srv.tick(false, &mut srv_out).unwrap();
                while let Some(c) = srv_out.pop() {
                    assert!(c.token.0 == 1 && c.result >= 1, "{c:?}");
                    accept_armed = c.more;
                    accepted.push(Sock(c.result as u32));
                }
            }
        }
        assert_eq!(accepted.len(), TOTAL);

        // Cancel ArmedAccept so only 1,000 armed receives remain in flight.
        if accept_armed {
            srv.submit(Token(2), Op::Cancel { target: Token(1) })
                .unwrap();
            let (mut c_done, mut a_done) = (false, false);
            while !(c_done && a_done) {
                srv.tick(true, &mut srv_out).unwrap();
                while let Some(c) = srv_out.pop() {
                    if c.token.0 == 2 {
                        c_done = true;
                    } else if c.token.0 == 1 && !c.more {
                        a_done = true;
                    }
                }
            }
        }

        // Arm receive on all 1,000 idle connections; zero ring buffers must be held.
        for (k, &s) in accepted.iter().enumerate() {
            srv.submit(Token(10_000 + k as u64), Op::ArmedRecv { sock: s })
                .unwrap();
            recv_armed[k] = true;
        }
        srv.tick(false, &mut srv_out).unwrap();
        assert!(srv_out.is_empty(), "idle connections held a ring buffer");

        // Send 16 bytes across c0[0..16] proving all 16 ring buffers are free.
        for k in 0..16u32 {
            c0.buf(BufId(0))[0] = k as u8;
            c0.submit(
                Token(20_000 + k as u64),
                Op::Send {
                    sock: Sock(k),
                    buf: BufId(0),
                    off: 0,
                    len: 1,
                    zero_copy: false,
                },
            )
            .unwrap();
        }
        let mut sent = 0;
        let mut recvd = 0;
        let mut held = [None; 16];
        let mut spins = 0u32;
        while sent < 16 || recvd < 16 {
            spins += 1;
            assert!(spins < 500_000, "send16 stalled: sent={sent} recvd={recvd}");
            c0.tick(false, &mut c_out).unwrap();
            while let Some(c) = c_out.pop() {
                assert_eq!(c.result, 1);
                sent += 1;
            }
            srv.tick(false, &mut srv_out).unwrap();
            while let Some(c) = srv_out.pop() {
                let idx = (c.token.0 - 10_000) as usize;
                recv_armed[idx] = c.more;
                assert_eq!(c.result, 1);
                let b = c.buf.unwrap();
                held[recvd] = Some(b);
                recvd += 1;
            }
        }
        for b in held.into_iter().flatten() {
            srv.give_back(b);
        }

        // Send 16 more across c1[484..500] to confirm all 16 returned and usable.
        for k in 0..16u32 {
            c1.buf(BufId(0))[0] = (0x80 + k) as u8;
            c1.submit(
                Token(21_000 + k as u64),
                Op::Send {
                    sock: Sock(484 + k),
                    buf: BufId(0),
                    off: 0,
                    len: 1,
                    zero_copy: false,
                },
            )
            .unwrap();
        }
        let (mut sent2, mut recvd2, mut spins2) = (0, 0, 0u32);
        while sent2 < 16 || recvd2 < 16 {
            spins2 += 1;
            assert!(
                spins2 < 500_000,
                "send16_2 stalled: sent2={sent2} recvd2={recvd2}"
            );
            c1.tick(false, &mut c_out).unwrap();
            while let Some(c) = c_out.pop() {
                assert_eq!(c.result, 1);
                sent2 += 1;
            }
            srv.tick(false, &mut srv_out).unwrap();
            while let Some(c) = srv_out.pop() {
                let idx = (c.token.0 - 10_000) as usize;
                recv_armed[idx] = c.more;
                assert_eq!(c.result, 1);
                srv.give_back(c.buf.unwrap());
                recvd2 += 1;
            }
        }

        // Close accepted sockets in batches of 16 (since 1,000/1,024 ops slots are occupied).
        for (chunk_idx, chunk) in accepted.chunks(16).enumerate() {
            let base = chunk_idx * 16;
            let mut need_close = chunk.len();
            let mut need_cancel = 0usize;
            for (o, &s) in chunk.iter().enumerate() {
                if recv_armed[base + o] {
                    need_cancel += 1;
                }
                srv.submit(Token(90_000 + o as u64), Op::Close { sock: s })
                    .unwrap();
            }
            while need_close > 0 || need_cancel > 0 {
                srv.tick(true, &mut srv_out).unwrap();
                while let Some(c) = srv_out.pop() {
                    if c.token.0 >= 90_000 {
                        need_close -= 1;
                    } else if (10_000..11_000).contains(&c.token.0) && !c.more {
                        let idx = (c.token.0 - 10_000) as usize;
                        if recv_armed[idx] {
                            recv_armed[idx] = false;
                            need_cancel -= 1;
                        }
                    }
                }
            }
        }
        srv.submit(Token(99_999), Op::Close { sock: Sock(0) })
            .unwrap();
        srv.tick(true, &mut srv_out).unwrap();
        assert!(srv_out.pop().is_some());
        let c_socks: [Sock; HALF] = core::array::from_fn(|k| Sock(k as u32));
        close_batch(&mut c0, &mut c_out, &c_socks);
        close_batch(&mut c1, &mut c_out, &c_socks);
    });
}
