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
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();

    for i in 0..2u32 {
        io.submit(
            Token::new(10 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().filter(|c| c.token().get() == 1).count() == 2
            && v.iter().filter(|c| c.token().get() >= 10).count() == 2
    })
    .unwrap();

    io.submit(
        Token::new(12),
        Op::Connect {
            sock: Sock::new(3),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token().get() == 1 && !c.more())
            && v.iter().any(|c| c.token().get() == 12)
    })
    .unwrap();
    let end = cs
        .iter()
        .find(|c| c.token().get() == 1 && !c.more())
        .unwrap();
    assert!(
        end.result() == -ENFILE || end.result() == -EMFILE,
        "unexpected range-full error: {}",
        end.result()
    );
    let c3 = cs.iter().find(|c| c.token().get() == 12).unwrap();
    if c3.result() >= 0 {
        io.submit(
            Token::new(13),
            Op::Recv {
                sock: Sock::new(3),
                buf: BufId::new(0),
                off: 0,
                len: 64,
            },
        )
        .unwrap();
        let rc = drive_until(&mut io, &mut out, 40, |v| {
            v.iter().any(|c| c.token().get() == 13)
        })
        .unwrap();
        assert!(
            rc[0].result() < 0,
            "dropped connection must be reset on I/O: {rc:?}"
        );
    }
}

#[cfg(feature = "sim")]
#[test]
fn t2_f21_2_rearm_accept_after_closing_slot_accepts_next_client() {
    let mut io = SimIo::new(armed_limits(2, 4), 2112, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();

    for i in 0..2u32 {
        io.submit(
            Token::new(10 + u64::from(i)),
            Op::Connect {
                sock: Sock::new(1 + i),
                addr,
            },
        )
        .unwrap();
    }
    let cs = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().filter(|c| c.token().get() == 1).count() == 2
    })
    .unwrap();
    let first_slot = Sock::new(
        u32::try_from(cs.iter().find(|c| c.token().get() == 1).unwrap().result()).unwrap(),
    );

    io.submit(
        Token::new(12),
        Op::Connect {
            sock: Sock::new(3),
            addr,
        },
    )
    .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token().get() == 1 && !c.more())
    })
    .unwrap();

    io.submit(Token::new(20), Op::Close { sock: first_slot })
        .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 20, |v| {
        v.iter().any(|c| c.token().get() == 20)
    })
    .unwrap();

    io.submit(
        Token::new(21),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    io.submit(
        Token::new(22),
        Op::Connect {
            sock: Sock::new(4),
            addr,
        },
    )
    .unwrap();
    let reaccepted = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token().get() == 21) && v.iter().any(|c| c.token().get() == 22)
    })
    .unwrap();
    let accepted = reaccepted.iter().find(|c| c.token().get() == 21).unwrap();
    assert!(accepted.more());
    assert_eq!(u32::try_from(accepted.result()).unwrap(), first_slot.get());
}

/// Accepts `Sock(2)` from a `Sock(1)` connect.
#[cfg(feature = "sim")]
fn accept_pair(io: &mut SimIo, out: &mut Completions) {
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
    let _: Vec<longshore::io::Completion> = drive_until(io, out, 30, |v| v.len() == 2).unwrap();
}

#[cfg(feature = "sim")]
fn send_from_client(io: &mut SimIo, token: Token, msg: &[u8]) {
    write_pool_bytes(io, BufId::new(0), 0, msg);
    io.submit(
        token,
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: u32::try_from(msg.len()).unwrap(),
            zero_copy: false,
        },
    )
    .unwrap();
}

/// Receives two messages on the armed receive (token 3) and holds both ring buffers.
#[cfg(feature = "sim")]
fn hold_two_ring_buffers(io: &mut SimIo, out: &mut Completions) -> Vec<BufId> {
    let mut held = Vec::new();
    for i in 0..2u8 {
        let msg = [b'1' + i; 10];
        send_from_client(io, Token::new(10 + u64::from(i)), &msg);
        let cs = drive_until(io, out, 30, |v| v.iter().any(|c| c.token().get() == 3)).unwrap();
        held.push(
            cs.iter()
                .find(|c| c.token().get() == 3)
                .unwrap()
                .buf()
                .unwrap(),
        );
    }
    held
}

#[cfg(feature = "sim")]
#[test]
fn t2_f21_3_empty_buffer_ring_returns_enobufs_and_preserves_bytes_on_rearm() {
    let mut io = SimIo::new(armed_limits(4, 2), 2113, Faults::default());
    let mut out = Completions::new(16);
    accept_pair(&mut io, &mut out);

    io.submit(Token::new(3), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();
    let held = hold_two_ring_buffers(&mut io, &mut out);

    send_from_client(&mut io, Token::new(12), b"third-chunk");
    let cs = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token().get() == 3 && !c.more())
    })
    .unwrap();
    let err = cs.iter().find(|c| c.token().get() == 3).unwrap();
    assert_eq!(err.result(), -ENOBUFS);
    assert!(err.buf().is_none());

    for id in held {
        io.give_back(id);
    }
    io.submit(Token::new(30), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();
    let redelivered = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token().get() == 30)
    })
    .unwrap();
    let delivered = redelivered.iter().find(|c| c.token().get() == 30).unwrap();
    assert_eq!(delivered.result(), 11);
    let id = delivered.buf().unwrap();
    assert_eq!(read_pool_bytes(&mut io, id, 0, 11), b"third-chunk");
    io.give_back(id);
}

#[cfg(unix)]
#[test]
fn t2_f21_4_ring_buffers_above_32768_rejected_cleanly() {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(1)
        .with_buffer_size(4096)
        .with_ops_max(4)
        .with_ring_buffers(32_769)
        .with_ring_buffer_size(4096);
    let rejected = match std::panic::catch_unwind(|| PosixIo::new(limits)) {
        Err(_) | Ok(Err(_)) => true,
        Ok(Ok(_)) => false,
    };
    assert!(rejected, "ring_buffers > 32768 must be rejected");
}

#[cfg(feature = "sim")]
#[test]
fn t2_f21_5_armed_ops_count_against_ops_max_busy_limit() {
    let limits = Limits::BASE
        .with_sockets_max(8)
        .with_buffers(2)
        .with_buffer_size(512)
        .with_ops_max(2)
        .with_accept_range_start(4)
        .with_accept_range_len(4)
        .with_ring_buffers(2)
        .with_ring_buffer_size(512);
    let mut io = SimIo::new(limits, 2115, Faults::default());
    let mut out = Completions::new(8);
    let addr = io.listen(Sock::new(0), ADDR).unwrap();
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
            addr,
        },
    )
    .unwrap();
    assert_eq!(
        io.submit(Token::new(3), Op::Timeout { nanos: 0 }),
        Err(Busy)
    );
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 30, |v| {
        v.iter().any(|c| c.token().get() == 2)
    })
    .unwrap();
    io.submit(Token::new(4), Op::ArmedRecv { sock: Sock::new(4) })
        .unwrap();
    assert_eq!(
        io.submit(Token::new(5), Op::Timeout { nanos: 0 }),
        Err(Busy)
    );
}
