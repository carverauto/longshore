// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial real-kernel `UringIo` tests (`#[cfg(target_os = "linux")]`).

use super::super::harness::{
    drive_until, submit_and_wait_one, write_nul_path, write_pool_bytes, TempDir,
};
use longshore::io::uring::UringIo;
use longshore::io::{
    BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token, ECANCELED,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

#[test]
fn uring_close_and_cancel_connect_with_tcp_opts_and_segment_probe() {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let mut ring = UringIo::new(Limits {
        sockets_max: 4,
        buffers: 1,
        buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    })
    .unwrap();
    ring.no_delay(true);
    ring.keep_alive(true);
    ring.measure_segment_sizes(true);
    let mut out = Completions::new(8);

    // 1. Cancel Connect while State::ConnectStep has probe_fd >= 0.
    let blackhole: SocketAddr = "192.0.2.1:80".parse().unwrap();
    ring.submit(
        Token(1),
        Op::Connect {
            sock: Sock(0),
            addr: blackhole,
        },
    )
    .unwrap();
    ring.submit(Token(2), Op::Cancel { target: Token(1) })
        .unwrap();
    let cs = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    let r1 = cs.iter().find(|c| c.token == Token(1)).unwrap().result;
    assert!(r1 < 0, "expected cancelled/failed connect, got {r1}");

    // 2. Close Sock(1) while Connect is in State::ConnectStep -> no double-close when sock_closed == true.
    ring.submit(
        Token(3),
        Op::Connect {
            sock: Sock(1),
            addr: blackhole,
        },
    )
    .unwrap();
    ring.submit(Token(4), Op::Close { sock: Sock(1) }).unwrap();
    let cs2 = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(3)) && v.iter().any(|c| c.token == Token(4))
    })
    .unwrap();
    assert_eq!(cs2.iter().find(|c| c.token == Token(4)).unwrap().result, 0);
    assert!(cs2.iter().find(|c| c.token == Token(3)).unwrap().result < 0);

    // 3. Slots 0 and 1 remain cleanly reusable for real loopback Connect.
    let c = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(5),
        Op::Connect {
            sock: Sock(0),
            addr,
        },
        200,
    )
    .unwrap();
    assert!(c.result >= 0);
    let _ = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(6),
        Op::Close { sock: Sock(0) },
        200,
    )
    .unwrap();
    drop(listener);
}

#[test]
fn uring_send_zc_close_cancel_and_quiesce_drop_mixed_slots() {
    let tmp = TempDir::new("uring_quiesce_adv").unwrap();
    let fpath = tmp.file_path("quiesce.dat");
    let mut ring = UringIo::new(Limits {
        sockets_max: 8,
        accept_range_start: 4,
        accept_range_len: 2,
        buffers: 2,
        buffer_size: 4096,
        ring_buffers: 4,
        ring_buffer_size: 256,
        files_max: 2,
        ops_max: 16,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(16);

    let addr = ring
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    ring.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    ring.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    let srv = Sock(cs.iter().find(|c| c.token == Token(1)).unwrap().result as u32);

    // Arm Recv on srv, send zero-copy, then cancel/close and test quiesce on drop with mixed active ops.
    ring.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();
    write_pool_bytes(&mut ring, BufId(0), 0, b"zc-payload");
    ring.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 10,
            zero_copy: true,
        },
    )
    .unwrap();
    let _ = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(4)) && v.iter().any(|c| c.token == Token(3))
    })
    .unwrap();
    while let Some(c) = out.pop() {
        if let Some(id) = c.buf {
            ring.give_back(id);
        }
    }

    // Leave Timer, ArmedAccept, ArmedRecv, FileSlotOpen, and SendZc in-flight and drop ring (exercises quiesce).
    let plen = write_nul_path(&mut ring, BufId(1), 0, fpath.to_str().unwrap());
    ring.submit(
        Token(5),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(1),
            path_off: 0,
            path_len: plen,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    ring.submit(
        Token(6),
        Op::Timeout {
            nanos: 5_000_000_000,
        },
    )
    .unwrap();
    ring.submit(
        Token(7),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 10,
            zero_copy: true,
        },
    )
    .unwrap();
    drop(ring);
}

#[test]
fn uring_unix_armed_accept_recv_close_and_shutdown_eof() {
    let tmp = TempDir::new("uring_unix_adv").unwrap();
    let sock_path = tmp.file_path("adv.sock");
    let path_str = sock_path.to_str().unwrap();

    let mut ring = UringIo::new(Limits {
        sockets_max: 6,
        accept_range_start: 3,
        accept_range_len: 2,
        buffers: 2,
        buffer_size: 512,
        ring_buffers: 4,
        ring_buffer_size: 256,
        ops_max: 8,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(16);
    ring.listen_unix(Sock(0), path_str).unwrap();

    // 1. ConnectUnix cancelled by Cancel and Close.
    let plen = write_nul_path(&mut ring, BufId(0), 0, path_str);
    ring.submit(
        Token(1),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: plen,
        },
    )
    .unwrap();
    ring.submit(Token(2), Op::Cancel { target: Token(1) })
        .unwrap();
    let cs = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(1)) && v.iter().any(|c| c.token == Token(2))
    })
    .unwrap();
    let r1 = cs.iter().find(|c| c.token == Token(1)).unwrap().result;
    assert!(r1 == 0 || r1 == -ECANCELED);
    let _ = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(3),
        Op::Close { sock: Sock(1) },
        200,
    )
    .unwrap();

    // 2. ArmedAccept -> ConnectUnix -> ArmedRecv -> Send -> Close accepted Unix socket -> peer EOF.
    ring.submit(Token(10), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    ring.submit(
        Token(11),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: plen,
        },
    )
    .unwrap();
    let cs2 = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(10)) && v.iter().any(|c| c.token == Token(11))
    })
    .unwrap();
    let srv = Sock(cs2.iter().find(|c| c.token == Token(10)).unwrap().result as u32);
    assert_eq!(cs2.iter().find(|c| c.token == Token(11)).unwrap().result, 0);

    ring.submit(Token(12), Op::ArmedRecv { sock: srv }).unwrap();
    write_pool_bytes(&mut ring, BufId(1), 0, b"unix-msg");
    ring.submit(
        Token(13),
        Op::Send {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 8,
            zero_copy: false,
        },
    )
    .unwrap();
    let cs3 = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(12)) && v.iter().any(|c| c.token == Token(13))
    })
    .unwrap();
    let recv_c = cs3.iter().find(|c| c.token == Token(12)).unwrap();
    assert_eq!(recv_c.result, 8);
    if let Some(id) = recv_c.buf {
        ring.give_back(id);
    }

    // Close accepted Unix socket (srv) -> peer Recv on Sock(1) must observe clean 0 EOF (shutdown-before-close).
    ring.submit(
        Token(14),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    ring.submit(Token(15), Op::Close { sock: srv }).unwrap();
    let cs4 = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token == Token(14)) && v.iter().any(|c| c.token == Token(15))
    })
    .unwrap();
    assert_eq!(cs4.iter().find(|c| c.token == Token(14)).unwrap().result, 0);
}

#[test]
fn uring_zero_length_buffer_class_registers_cleanly() {
    let lim = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 0,
        aligned_buffers: 1,
        aligned_buffer_size: 4096,
        block_size: 4096,
        files_max: 1,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(4);
    let c = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(1),
        Op::Timeout { nanos: 100_000 },
        200,
    )
    .unwrap();
    assert_eq!(c.result, 0);
}

#[cfg(feature = "fault-injection")]
#[test]
fn uring_fault_injection_tick_fails_and_ring_lost() {
    use longshore::io::uring::Fault;

    let lim = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(4);
    ring.inject(Fault::TickFails);
    assert!(ring.tick(false, &mut out).is_err());

    // After TickFails, ring still recovers and executes Timeout cleanly.
    let c = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(1),
        Op::Timeout { nanos: 100_000 },
        200,
    )
    .unwrap();
    assert_eq!(c.result, 0);

    // RingLost with an active op drops safely without panicking.
    ring.submit(
        Token(2),
        Op::Timeout {
            nanos: 5_000_000_000,
        },
    )
    .unwrap();
    ring.inject(Fault::RingLost);
    assert!(ring.tick(true, &mut out).is_err());
    drop(ring);
}
