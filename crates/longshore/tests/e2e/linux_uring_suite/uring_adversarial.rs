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
    BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token, ECANCELED,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

#[test]
fn uring_close_and_cancel_connect_with_tcp_opts_and_segment_probe() {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let mut ring = UringIo::new(
        Limits::BASE
            .with_sockets_max(4)
            .with_buffers(1)
            .with_buffer_size(256)
            .with_ops_max(8),
    )
    .unwrap();
    ring.no_delay(true);
    ring.keep_alive(true);
    ring.measure_segment_sizes(true);
    let mut out = Completions::new(8);

    // 1. Cancel Connect while State::ConnectStep has probe_fd >= 0.
    let blackhole: SocketAddr = "192.0.2.1:80".parse().unwrap();
    ring.submit(
        Token::new(1),
        Op::Connect {
            sock: Sock::new(0),
            addr: blackhole,
        },
    )
    .unwrap();
    ring.submit(
        Token::new(2),
        Op::Cancel {
            target: Token::new(1),
        },
    )
    .unwrap();
    let cs = drive_both(&mut ring, &mut out, 1, 2);
    let r1 = found(&cs, 1).result();
    assert!(r1 < 0, "expected cancelled/failed connect, got {r1}");

    // 2. Close Sock(1) while Connect is in State::ConnectStep -> no double-close when sock_closed == true.
    ring.submit(
        Token::new(3),
        Op::Connect {
            sock: Sock::new(1),
            addr: blackhole,
        },
    )
    .unwrap();
    ring.submit(Token::new(4), Op::Close { sock: Sock::new(1) })
        .unwrap();
    let cs2 = drive_both(&mut ring, &mut out, 3, 4);
    assert_eq!(found(&cs2, 4).result(), 0);
    assert!(found(&cs2, 3).result() < 0);

    // 3. Slots 0 and 1 remain cleanly reusable for real loopback Connect.
    let c = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token::new(5),
        Op::Connect {
            sock: Sock::new(0),
            addr,
        },
        200,
    )
    .unwrap();
    assert!(c.result() >= 0);
    let _: Completion = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token::new(6),
        Op::Close { sock: Sock::new(0) },
        200,
    )
    .unwrap();
    drop(listener);
}

/// Drives until both tokens have completed and returns everything collected.
fn drive_both(ring: &mut UringIo, out: &mut Completions, a: u64, b: u64) -> Vec<Completion> {
    drive_until(ring, out, 200, |v| {
        v.iter().any(|c| c.token() == Token::new(a)) && v.iter().any(|c| c.token() == Token::new(b))
    })
    .unwrap()
}

fn found(cs: &[Completion], token: u64) -> &Completion {
    cs.iter().find(|c| c.token() == Token::new(token)).unwrap()
}

/// Accepts one loopback TCP connection from `Sock(1)` and returns the accepted slot.
fn accept_tcp_pair(ring: &mut UringIo, out: &mut Completions) -> Sock {
    let addr = ring
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    ring.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    ring.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_both(ring, out, 1, 2);
    Sock::new(u32::try_from(found(&cs, 1).result()).unwrap())
}

fn send_op(buf: BufId, len: u32, zero_copy: bool) -> Op {
    Op::Send {
        sock: Sock::new(1),
        buf,
        off: 0,
        len,
        zero_copy,
    }
}

/// Leaves a file open, a long timer, and a zero-copy send in flight next to the armed ops.
fn leave_mixed_ops_in_flight(ring: &mut UringIo, fpath: &std::path::Path) {
    let plen = write_nul_path(ring, BufId::new(1), 0, fpath.to_str().unwrap());
    ring.submit(
        Token::new(5),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(1),
            path_off: 0,
            path_len: plen,
            flags: OpenFlags::NONE
                .with_read(true)
                .with_write(true)
                .with_create(true),
        },
    )
    .unwrap();
    ring.submit(
        Token::new(6),
        Op::Timeout {
            nanos: 5_000_000_000,
        },
    )
    .unwrap();
    ring.submit(Token::new(7), send_op(BufId::new(0), 10, true))
        .unwrap();
}

#[test]
fn uring_send_zc_close_cancel_and_quiesce_drop_mixed_slots() {
    let tmp = TempDir::new("uring_quiesce_adv").unwrap();
    let fpath = tmp.file_path("quiesce.dat");
    let mut ring = UringIo::new(
        Limits::BASE
            .with_sockets_max(8)
            .with_accept_range_start(4)
            .with_accept_range_len(2)
            .with_buffers(2)
            .with_buffer_size(4096)
            .with_ring_buffers(4)
            .with_ring_buffer_size(256)
            .with_files_max(2)
            .with_ops_max(16),
    )
    .unwrap();
    let mut out = Completions::new(16);
    let srv = accept_tcp_pair(&mut ring, &mut out);

    // Arm Recv on srv, send zero-copy, then cancel/close and test quiesce on drop with mixed active ops.
    ring.submit(Token::new(3), Op::ArmedRecv { sock: srv })
        .unwrap();
    write_pool_bytes(&mut ring, BufId::new(0), 0, b"zc-payload");
    ring.submit(Token::new(4), send_op(BufId::new(0), 10, true))
        .unwrap();
    let _: Vec<Completion> = drive_both(&mut ring, &mut out, 4, 3);
    while let Some(c) = out.pop() {
        if let Some(id) = c.buf() {
            ring.give_back(id);
        }
    }

    // Leave Timer, ArmedAccept, ArmedRecv, FileSlotOpen, and SendZc in-flight and drop ring (exercises quiesce).
    leave_mixed_ops_in_flight(&mut ring, &fpath);
    drop(ring);
}

fn connect_unix(plen: u32) -> Op {
    Op::ConnectUnix {
        sock: Sock::new(1),
        path_buf: BufId::new(0),
        path_off: 0,
        path_len: plen,
    }
}

/// A Unix connect cancelled in flight ends cleanly or as `-ECANCELED`, and its slot closes.
fn cancel_unix_connect(ring: &mut UringIo, out: &mut Completions, plen: u32) {
    ring.submit(Token::new(1), connect_unix(plen)).unwrap();
    ring.submit(
        Token::new(2),
        Op::Cancel {
            target: Token::new(1),
        },
    )
    .unwrap();
    let cs = drive_both(ring, out, 1, 2);
    let r1 = found(&cs, 1).result();
    assert!(r1 == 0 || r1 == -ECANCELED);
    let _: Completion = submit_and_wait_one(
        ring,
        out,
        Token::new(3),
        Op::Close { sock: Sock::new(1) },
        200,
    )
    .unwrap();
}

/// Accepts a Unix connection, then receives one 8-byte message on it through a ring buffer.
fn unix_accept_and_recv(ring: &mut UringIo, out: &mut Completions, plen: u32) -> Sock {
    ring.submit(
        Token::new(10),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    ring.submit(Token::new(11), connect_unix(plen)).unwrap();
    let cs2 = drive_both(ring, out, 10, 11);
    let srv = Sock::new(u32::try_from(found(&cs2, 10).result()).unwrap());
    assert_eq!(found(&cs2, 11).result(), 0);

    ring.submit(Token::new(12), Op::ArmedRecv { sock: srv })
        .unwrap();
    write_pool_bytes(ring, BufId::new(1), 0, b"unix-msg");
    ring.submit(Token::new(13), send_op(BufId::new(1), 8, false))
        .unwrap();
    let cs3 = drive_both(ring, out, 12, 13);
    let recv_c = found(&cs3, 12);
    assert_eq!(recv_c.result(), 8);
    if let Some(id) = recv_c.buf() {
        ring.give_back(id);
    }
    srv
}

#[test]
fn uring_unix_armed_accept_recv_close_and_shutdown_eof() {
    let tmp = TempDir::new("uring_unix_adv").unwrap();
    let sock_path = tmp.file_path("adv.sock");
    let path_str = sock_path.to_str().unwrap();

    let mut ring = UringIo::new(
        Limits::BASE
            .with_sockets_max(6)
            .with_accept_range_start(3)
            .with_accept_range_len(2)
            .with_buffers(2)
            .with_buffer_size(512)
            .with_ring_buffers(4)
            .with_ring_buffer_size(256)
            .with_ops_max(8),
    )
    .unwrap();
    let mut out = Completions::new(16);
    ring.listen_unix(Sock::new(0), path_str).unwrap();

    // 1. ConnectUnix cancelled by Cancel and Close.
    let plen = write_nul_path(&mut ring, BufId::new(0), 0, path_str);
    cancel_unix_connect(&mut ring, &mut out, plen);

    // 2. ArmedAccept -> ConnectUnix -> ArmedRecv -> Send -> Close accepted Unix socket -> peer EOF.
    let srv = unix_accept_and_recv(&mut ring, &mut out, plen);

    // Close accepted Unix socket (srv) -> peer Recv on Sock(1) must observe clean 0 EOF (shutdown-before-close).
    ring.submit(
        Token::new(14),
        Op::Recv {
            sock: Sock::new(1),
            buf: BufId::new(1),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    ring.submit(Token::new(15), Op::Close { sock: srv })
        .unwrap();
    let cs4 = drive_both(&mut ring, &mut out, 14, 15);
    assert_eq!(found(&cs4, 14).result(), 0);
}

#[test]
fn uring_zero_length_buffer_class_registers_cleanly() {
    let lim = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(0)
        .with_aligned_buffers(1)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_files_max(1)
        .with_ops_max(4);
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(4);
    let c = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token::new(1),
        Op::Timeout { nanos: 100_000 },
        200,
    )
    .unwrap();
    assert_eq!(c.result(), 0);
}

#[cfg(feature = "fault-injection")]
#[test]
fn uring_fault_injection_tick_fails_and_ring_lost() {
    use longshore::io::uring::Fault;

    let lim = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(256)
        .with_ops_max(4);
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(4);
    ring.inject(Fault::TickFails);
    assert!(ring.tick(false, &mut out).is_err());

    // After TickFails, ring still recovers and executes Timeout cleanly.
    let c = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token::new(1),
        Op::Timeout { nanos: 100_000 },
        200,
    )
    .unwrap();
    assert_eq!(c.result(), 0);

    // RingLost with an active op drops safely without panicking.
    ring.submit(
        Token::new(2),
        Op::Timeout {
            nanos: 5_000_000_000,
        },
    )
    .unwrap();
    ring.inject(Fault::RingLost);
    assert!(ring.tick(true, &mut out).is_err());
    drop(ring);
}
