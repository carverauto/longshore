// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 21 Tier 2 E2E tests: range full, re-arming, ring exhaustion & limits.

#[cfg(feature = "sim")]
use super::super::harness::{drive_until, read_pool_bytes, write_pool_bytes};
#[cfg(feature = "sim")]
use super::f21_armed_ops_t1::{armed_limits, ADDR};
#[cfg(unix)]
use longshore::io::posix::PosixIo;
#[cfg(feature = "sim")]
use longshore::io::Busy;
#[cfg(unix)]
use longshore::io::Limits;
#[cfg(feature = "sim")]
use longshore::io::{BufId, Completions, Io, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

#[cfg(feature = "sim")]
const ENFILE: i32 = 23;
#[cfg(feature = "sim")]
const EMFILE: i32 = 24;
#[cfg(feature = "sim")]
const ENOBUFS: i32 = 105;

#[cfg(feature = "sim")]
#[test]
fn t2_f21_1_armed_accept_range_full_terminates_with_enfile_or_emfile() {
    let mut io = SimIo::new(armed_limits(2, 4), 2111, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock(0), ADDR).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    for i in 0..2u32 {
        io.submit(
            Token(10 + i as u64),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().filter(|c| c.token == Token(1)).count() == 2
            && v.iter().filter(|c| c.token.0 >= 10).count() == 2
    })
    .unwrap();

    io.submit(
        Token(12),
        Op::Connect {
            sock: Sock(3),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token == Token(1) && !c.more) && v.iter().any(|c| c.token == Token(12))
    })
    .unwrap();
    let end = cs.iter().find(|c| c.token == Token(1) && !c.more).unwrap();
    assert!(
        end.result == -ENFILE || end.result == -EMFILE,
        "unexpected range-full error: {}",
        end.result
    );
    let c3 = cs.iter().find(|c| c.token == Token(12)).unwrap();
    if c3.result >= 0 {
        io.submit(
            Token(13),
            Op::Recv {
                sock: Sock(3),
                buf: BufId(0),
                off: 0,
                len: 64,
            },
        )
        .unwrap();
        let rc = drive_until(&mut io, &mut out, 40, |v| {
            v.iter().any(|c| c.token == Token(13))
        })
        .unwrap();
        assert!(
            rc[0].result < 0,
            "dropped connection must be reset on I/O: {rc:?}"
        );
    }
}

#[cfg(feature = "sim")]
#[test]
fn t2_f21_2_rearm_accept_after_closing_slot_accepts_next_client() {
    let mut io = SimIo::new(armed_limits(2, 4), 2112, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock(0), ADDR).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();

    for i in 0..2u32 {
        io.submit(
            Token(10 + i as u64),
            Op::Connect {
                sock: Sock(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().filter(|c| c.token == Token(1)).count() == 2
    })
    .unwrap();
    let first_slot = Sock(cs.iter().find(|c| c.token == Token(1)).unwrap().result as u32);

    io.submit(
        Token(12),
        Op::Connect {
            sock: Sock(3),
            addr,
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token == Token(1) && !c.more)
    })
    .unwrap();

    io.submit(Token(20), Op::Close { sock: first_slot })
        .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 20, |v| {
        v.iter().any(|c| c.token == Token(20))
    })
    .unwrap();

    io.submit(Token(21), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(22),
        Op::Connect {
            sock: Sock(4),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token == Token(21)) && v.iter().any(|c| c.token == Token(22))
    })
    .unwrap();
    let accepted = cs.iter().find(|c| c.token == Token(21)).unwrap();
    assert!(accepted.more);
    assert_eq!(accepted.result as u32, first_slot.0);
}

#[cfg(feature = "sim")]
#[test]
fn t2_f21_3_empty_buffer_ring_returns_enobufs_and_preserves_bytes_on_rearm() {
    let mut io = SimIo::new(armed_limits(4, 2), 2113, Faults::default());
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
    let _: Vec<longshore::io::Completion> =
        drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let mut held = Vec::new();
    for i in 0..2u8 {
        let msg = [b'1' + i; 10];
        write_pool_bytes(&mut io, BufId(0), 0, &msg);
        io.submit(
            Token(10 + i as u64),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 10,
                zero_copy: false,
            },
        )
        .unwrap();
        let cs = drive_until(&mut io, &mut out, 30, |v| {
            v.iter().any(|c| c.token == Token(3))
        })
        .unwrap();
        held.push(
            cs.iter()
                .find(|c| c.token == Token(3))
                .unwrap()
                .buf
                .unwrap(),
        );
    }

    write_pool_bytes(&mut io, BufId(0), 0, b"third-chunk");
    io.submit(
        Token(12),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 11,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more)
    })
    .unwrap();
    let err = cs.iter().find(|c| c.token == Token(3)).unwrap();
    assert_eq!(err.result, -ENOBUFS);
    assert!(err.buf.is_none());

    for id in held {
        io.give_back(id);
    }
    io.submit(Token(30), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let cs = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token == Token(30))
    })
    .unwrap();
    let delivered = cs.iter().find(|c| c.token == Token(30)).unwrap();
    assert_eq!(delivered.result, 11);
    let id = delivered.buf.unwrap();
    assert_eq!(read_pool_bytes(&mut io, id, 0, 11), b"third-chunk");
    io.give_back(id);
}

#[cfg(unix)]
#[test]
fn t2_f21_4_ring_buffers_above_32768_rejected_cleanly() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 1,
        buffer_size: 4096,
        ops_max: 4,
        ring_buffers: 32_769,
        ring_buffer_size: 4096,
        ..Limits::BASE
    };
    let rejected = match std::panic::catch_unwind(|| PosixIo::new(limits)) {
        Err(_) | Ok(Err(_)) => true,
        Ok(Ok(_)) => false,
    };
    assert!(rejected, "ring_buffers > 32768 must be rejected");
}

#[cfg(feature = "sim")]
#[test]
fn t2_f21_5_armed_ops_count_against_ops_max_busy_limit() {
    let limits = Limits {
        sockets_max: 8,
        buffers: 2,
        buffer_size: 512,
        ops_max: 2,
        accept_range_start: 4,
        accept_range_len: 4,
        ring_buffers: 2,
        ring_buffer_size: 512,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 2115, Faults::default());
    let mut out = Completions::new(8);
    let addr = io.listen(Sock(0), ADDR).unwrap();
    io.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    assert_eq!(io.submit(Token(3), Op::Timeout { nanos: 0 }), Err(Busy));
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    io.submit(Token(4), Op::ArmedRecv { sock: Sock(4) })
        .unwrap();
    assert_eq!(io.submit(Token(5), Op::Timeout { nanos: 0 }), Err(Busy));
}
