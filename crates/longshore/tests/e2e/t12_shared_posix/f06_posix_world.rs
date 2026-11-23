// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 6: `Posix` world (`kqueue` & `epoll`) (`#41`).

#[cfg(unix)]
use crate::e2e::harness::{drive_until, posix_backend, submit_and_wait_one, write_pool_bytes};
#[cfg(unix)]
use longshore::io::alloc_guard::sealed;
#[cfg(unix)]
use longshore::io::scenario::echo;
#[cfg(unix)]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};

#[cfg(unix)]
pub(super) fn connect_pair<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    listener: Sock,
    s_sock: Sock,
    c_sock: Sock,
) {
    let addr = "127.0.0.1:0".parse().expect("addr");
    let bound = io.listen(listener, addr).expect("listen");
    io.submit(
        Token(9001),
        Op::Accept {
            listener,
            sock: s_sock,
        },
    )
    .unwrap();
    io.submit(
        Token(9002),
        Op::Connect {
            sock: c_sock,
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(io, out, 1_000, |v| v.len() == 2).expect("pair");
    assert!(cs.iter().all(|c| c.result == 0), "connect failed: {cs:?}");
}

#[cfg(unix)]
#[test]
fn t1_f06_1_posix_loopback_echo_300kb() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 4096,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    let addr = "127.0.0.1:0".parse().expect("addr");
    let stats = echo(&mut io, addr, 300_000, 4096, false, &mut out).expect("posix echo");
    assert!(stats.sends > 0 && stats.recvs > 0 && stats.ticks > 0);
}

/// How long a Send must stay pending before the test treats the socket as full.
#[cfg(unix)]
const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Ticks without blocking until a completion arrives or `window` passes.
#[cfg(unix)]
fn completion_within<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    window: std::time::Duration,
) -> Option<longshore::io::Completion> {
    let start = std::time::Instant::now();
    loop {
        io.tick(false, out).unwrap();
        if let Some(c) = out.pop() {
            return Some(c);
        }
        if start.elapsed() >= window {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(unix)]
#[test]
fn t1_f06_2_posix_bidirectional_send_and_recv_wait() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 65_536,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    connect_pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    // Fill Sock(2)'s send buffer until an Op::Send blocks waiting for writability.
    // One tick without a completion is not enough: on macOS the loopback socket
    // turns writable again within a fraction of a millisecond, several times,
    // while the kernel moves data into the peer's receive buffer and grows both
    // buffers. A Send counts as stalled only when it stays pending for `SETTLE`.
    io.buf(BufId(0)).fill(0x55);
    let mut stalled_send_tok = None;
    for step in 0..256u64 {
        let tok = Token(100 + step);
        io.submit(
            tok,
            Op::Send {
                sock: Sock(2),
                buf: BufId(0),
                off: 0,
                len: 65_536,
                zero_copy: false,
            },
        )
        .unwrap();
        if let Some(c) = completion_within(&mut io, &mut out, SETTLE) {
            assert_eq!(c.token, tok);
            assert!(c.result > 0, "send failed: {}", c.result);
        } else {
            stalled_send_tok = Some(tok);
            break;
        }
    }
    let mut send_tok = stalled_send_tok.expect("kernel send buffer should eventually backpressure");

    // Submit Recv on the same socket Sock(2) while Send is still waiting.
    io.submit(
        Token(500),
        Op::Recv {
            sock: Sock(2),
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
    .unwrap();
    // Both must now be pending. Under load the kernel can drain a "full"
    // loopback socket later than `SETTLE`, so a Send that completes anyway is
    // replaced by another until one stays pending: the test then starts from
    // a Send that is really waiting, however long the drain took.
    let mut both_pending = false;
    for step in 0..256u64 {
        match completion_within(&mut io, &mut out, SETTLE) {
            None => {
                both_pending = true;
                break;
            }
            Some(c) if c.token == send_tok => {
                assert!(c.result > 0, "send failed: {}", c.result);
                send_tok = Token(1_000 + step);
                io.submit(
                    send_tok,
                    Op::Send {
                        sock: Sock(2),
                        buf: BufId(0),
                        off: 0,
                        len: 65_536,
                        zero_copy: false,
                    },
                )
                .unwrap();
            }
            Some(c) => panic!(
                "unexpected completion: token {} result {}",
                c.token.0, c.result
            ),
        }
    }
    assert!(
        both_pending,
        "a Send and a Recv must both be pending on Sock(2)"
    );

    // Peer Sock(1) sends 5 bytes -> Sock(2)'s Recv must complete although a Send
    // on the same socket is pending.
    write_pool_bytes(&mut io, BufId(2), 0, b"reply");
    io.submit(
        Token(501),
        Op::Send {
            sock: Sock(1),
            buf: BufId(2),
            off: 0,
            len: 5,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 100, |v| {
        v.iter().any(|c| c.token == Token(500))
    })
    .expect("recv completes");
    assert!(cs.iter().any(|c| c.token == Token(500) && c.result == 5));
    assert_eq!(&io.buf(BufId(1))[..5], b"reply");
    // The stalled Send may complete here too, without Sock(1) reading: the
    // segment carrying the reply can also carry an acknowledgement that frees
    // room in Sock(2)'s send buffer. On macOS loopback that happened in about
    // 1 run in 40, so the test does not require the Send to still be pending.
    let mut send_done = cs.iter().any(|c| c.token == send_tok && c.result > 0);

    // Drain Sock(1) until Sock(2)'s stalled Send completes.
    for k in 0..256u64 {
        if send_done {
            break;
        }
        io.submit(
            Token(600 + k),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(2),
                off: 0,
                len: 65_536,
            },
        )
        .unwrap();
        let drained = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
        if drained.iter().any(|c| c.token == send_tok && c.result > 0) {
            send_done = true;
            break;
        }
    }
    assert!(send_done, "blocked Send should complete once socket drains");
}

#[cfg(unix)]
#[test]
fn t1_f06_3_posix_zero_copy_flag_copies_and_completes_once() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 8192,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    connect_pair(&mut io, &mut out, Sock(0), Sock(1), Sock(2));

    let payload: Vec<u8> = (0..8192).map(|i| (i & 0xff) as u8).collect();
    write_pool_bytes(&mut io, BufId(0), 0, &payload);
    let mut sent = 0usize;
    let mut tok = 10u64;
    while sent < payload.len() {
        let s = submit_and_wait_one(
            &mut io,
            &mut out,
            Token(tok),
            Op::Send {
                sock: Sock(2),
                buf: BufId(0),
                off: sent as u32,
                len: (payload.len() - sent) as u32,
                zero_copy: true,
            },
            100,
        )
        .unwrap();
        assert!(s.result > 0);
        sent += s.result as usize;
        tok += 1;
    }
    let mut recvd = Vec::new();
    while recvd.len() < payload.len() {
        let r = submit_and_wait_one(
            &mut io,
            &mut out,
            Token(tok),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 8192,
            },
            100,
        )
        .unwrap();
        assert!(r.result > 0);
        recvd.extend_from_slice(&io.buf(BufId(1))[..r.result as usize]);
        tok += 1;
    }
    assert_eq!(recvd, payload);
}

#[cfg(unix)]
#[test]
fn t1_f06_4_posix_cross_thread_waker_wakes_tick() {
    let limits = Limits {
        sockets_max: 1,
        buffers: 1,
        buffer_size: 64,
        ops_max: 2,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(4);
    let waker = io.waker().expect("posix waker");
    io.submit(Token(1), Op::AwaitWake).unwrap();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(15));
        waker.wake();
    });
    io.tick(true, &mut out).expect("tick");
    let c = out.pop().expect("wake completion");
    assert_eq!((c.token, c.result), (Token(1), 0));
    handle.join().unwrap();
}

#[cfg(unix)]
#[test]
fn t1_f06_5_posix_echo_under_alloc_guard_sealed() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 4096,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(8);
    let addr = "127.0.0.1:0".parse().expect("addr");
    let stats = sealed(|| echo(&mut io, addr, 128 * 1024, 4096, false, &mut out))
        .expect("sealed posix echo");
    assert!(stats.sends > 0 && stats.recvs > 0);
}
