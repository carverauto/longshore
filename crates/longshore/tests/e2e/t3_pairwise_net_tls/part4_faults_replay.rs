// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-18`–`T3-20`: armed `Recv` `-ENOBUFS` under
//! `Faults`, `power_loss` reset on armed ops/`Stream`, and dual-fault replay.

#[cfg(feature = "sim")]
use super::super::harness::{drive_until, read_pool_bytes, write_nul_path, write_pool_bytes};
#[cfg(feature = "sim")]
use super::net_limits;
#[cfg(feature = "sim")]
use longshore::io::{
    BufId, Completion, Completions, FileSlot, Io, Op, OpenFlags, Sock, Token, ECONNRESET, ENOBUFS,
};
#[cfg(feature = "sim")]
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};
#[cfg(feature = "sim")]
use longshore::stream::Stream;

/// Connects `Sock(1)` to `Sock(2)` through a plain accept on `addr`.
#[cfg(feature = "sim")]
fn accept_connect_pair(sim: &mut SimIo, out: &mut Completions, addr: std::net::SocketAddr) {
    sim.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    sim.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(sim, out, 1000, |v| v.len() == 2).unwrap();
}

/// Copies the bytes an armed recv completion names into `received`; returns its ring buffer.
#[cfg(feature = "sim")]
fn take_ring_bytes(sim: &mut SimIo, c: &Completion, received: &mut Vec<u8>) -> BufId {
    let id = c.buf().unwrap();
    received.extend_from_slice(&read_pool_bytes(
        sim,
        id,
        0,
        usize::try_from(c.result()).unwrap(),
    ));
    id
}

/// Collects armed recv `Token(3)` data, holding its ring buffers, until it ends;
/// returns the final result, if the recv ended within 64 ticks.
#[cfg(feature = "sim")]
fn recv_until_armed_end(
    sim: &mut SimIo,
    out: &mut Completions,
    received: &mut Vec<u8>,
    held: &mut Vec<BufId>,
) -> Option<i32> {
    let mut ended_with = None;
    for _ in 0..64 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 3 {
                if c.result() > 0 {
                    held.push(take_ring_bytes(sim, &c, received));
                }
                if !c.more() {
                    ended_with = Some(c.result());
                }
            }
        }
        if ended_with.is_some() {
            break;
        }
    }
    ended_with
}

/// Gives the held ring buffers back and re-arms recv as `Token(5)`, reading
/// until the payload is complete or the recv ends.
#[cfg(feature = "sim")]
fn rearm_after_enobufs(
    sim: &mut SimIo,
    out: &mut Completions,
    received: &mut Vec<u8>,
    held: &mut Vec<BufId>,
    payload_len: usize,
) {
    for id in held.drain(..) {
        sim.give_back(id);
    }
    sim.submit(Token::new(5), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();
    for _ in 0..64 {
        sim.tick(false, out).unwrap();
        let mut hit_reset = false;
        while let Some(c) = out.pop() {
            if c.token().get() == 5 {
                if c.result() > 0 {
                    let id = take_ring_bytes(sim, &c, received);
                    sim.give_back(id);
                }
                if !c.more() {
                    assert!(c.result() == -ECONNRESET || c.result() == -ENOBUFS || c.result() >= 0);
                    hit_reset = true;
                }
            }
        }
        if received.len() == payload_len || hit_reset {
            break;
        }
    }
}

#[cfg(feature = "sim")]
#[test]
fn t3_18_armed_recv_enobufs_under_sim_faults_and_give_back() {
    let limits = net_limits(8, 4, 2, 2, 64);
    let faults = Faults::NONE.with_partial_recv(300_000).with_reset(50_000);
    let mut sim = SimIo::new(limits, 318, faults);
    let mut out = Completions::new(16);
    let addr = sim
        .listen(Sock::new(0), "192.0.2.38:9318".parse().unwrap())
        .unwrap();
    accept_connect_pair(&mut sim, &mut out, addr);

    sim.submit(Token::new(3), Op::ArmedRecv { sock: Sock::new(2) })
        .unwrap();
    let payload: Vec<u8> = (0..180u8).collect();
    write_pool_bytes(&mut sim, BufId::new(0), 0, &payload);
    sim.submit(
        Token::new(4),
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: payload.len() as u32,
            zero_copy: false,
        },
    )
    .unwrap();

    let mut received = Vec::new();
    let mut held = Vec::new();
    let ended_with = recv_until_armed_end(&mut sim, &mut out, &mut received, &mut held);
    let end = ended_with.expect("armed recv must end on -ENOBUFS or -ECONNRESET");
    assert!(end == -ENOBUFS || end == -ECONNRESET || end >= 0);
    if end == -ENOBUFS {
        rearm_after_enobufs(&mut sim, &mut out, &mut received, &mut held, payload.len());
    }
    assert_eq!(received, payload[..received.len()]);
}

/// The simulated disk the fault and power-loss scenarios mount.
#[cfg(feature = "sim")]
fn small_disk() -> DiskConfig {
    DiskConfig::BASE
        .with_max_files(4)
        .with_max_dirs(4)
        .with_max_bytes_per_file(64 * 1024)
        .with_max_name_len(128)
        .with_write_log_entries(16)
        .with_write_log_bytes(32 * 1024)
        .with_read_error_sectors(4)
        .with_sector_size(512)
}

/// Listens on `listen_at`, arms accept as `accept`, connects `Sock(1)` as
/// `connect`, and returns the accepted server socket.
#[cfg(feature = "sim")]
fn armed_accept_connect(
    sim: &mut SimIo,
    out: &mut Completions,
    listen_at: &str,
    accept: Token,
    connect: Token,
) -> Sock {
    let addr = sim
        .listen(Sock::new(0), listen_at.parse().unwrap())
        .unwrap();
    sim.submit(
        accept,
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    sim.submit(
        connect,
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(sim, out, 1000, |v| v.len() == 2).unwrap();
    Sock::new(u32::try_from(cs.iter().find(|c| c.token() == accept).unwrap().result()).unwrap())
}

/// The op that opens `FileSlot(0)` read-write-create from the path in `path_buf`.
#[cfg(feature = "sim")]
fn open_rw_create(path_buf: BufId, path_len: u32) -> Op {
    Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf,
        path_off: 0,
        path_len,
        flags: OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true),
    }
}

#[cfg(feature = "sim")]
#[test]
fn t3_19_simio_power_loss_resets_armed_ops_and_stream_rebuilds() {
    let mut limits = net_limits(12, 6, 4, 4, 1024);
    limits = limits.with_files_max(4);
    let mut sim = SimIo::new(limits, 319, Faults::default());
    sim.setup_disk(small_disk());
    let mut out = Completions::new(16);
    let srv = armed_accept_connect(
        &mut sim,
        &mut out,
        "192.0.2.38:9318",
        Token::new(1),
        Token::new(2),
    );
    sim.submit(Token::new(3), Op::ArmedRecv { sock: srv })
        .unwrap();

    let plen = write_nul_path(&mut sim, limits.small(0), 0, "/t3_19.dat");
    sim.submit(Token::new(4), open_rw_create(limits.small(0), plen))
        .unwrap();

    sim.power_loss();
    sim.tick(false, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "power_loss drops armed ops and file ops without completions"
    );

    let srv2 = armed_accept_connect(
        &mut sim,
        &mut out,
        "192.0.2.39:9320",
        Token::new(10),
        Token::new(11),
    );
    let mut s = Stream::new(Sock::new(1), Token::new(12), Token::new(13));
    write_pool_bytes(&mut sim, BufId::new(0), 0, b"rebuilt");
    s.send(&mut sim, BufId::new(0), 0, 7, u32::MAX);
    sim.submit(Token::new(14), Op::ArmedRecv { sock: srv2 })
        .unwrap();
    let cs3 = drive_until(&mut sim, &mut out, 1000, |v| {
        v.iter().any(|c| c.token().get() == 14)
    })
    .unwrap();
    let r = cs3.iter().find(|c| c.token().get() == 14).unwrap();
    assert_eq!(
        read_pool_bytes(
            &mut sim,
            r.buf().unwrap(),
            0,
            usize::try_from(r.result()).unwrap()
        ),
        b"rebuilt"
    );
    sim.give_back(r.buf().unwrap());
}

/// A simulator with both network and disk faults switched on.
#[cfg(feature = "sim")]
fn dual_fault_sim(limits: longshore::io::Limits, seed: u64) -> SimIo {
    let net_f = Faults::NONE
        .with_short_send(200_000)
        .with_partial_recv(200_000)
        .with_delay(200_000);
    let disk_f = DiskFaults::NONE
        .with_short_write(200_000)
        .with_torn_reported(100_000)
        .with_bit_flip(100_000);
    let mut sim = SimIo::new(limits, seed, net_f);
    sim.setup_disk(small_disk());
    sim.set_disk_faults(disk_f);
    sim
}

/// Arms accept, connects, and opens the log file in one burst; logs every
/// completion and returns the accepted server socket.
#[cfg(feature = "sim")]
fn connect_and_open_logged(
    sim: &mut SimIo,
    out: &mut Completions,
    limits: longshore::io::Limits,
    log: &mut Vec<(u64, i32, bool)>,
) -> Sock {
    let addr = sim
        .listen(Sock::new(0), "192.0.2.40:9321".parse().unwrap())
        .unwrap();
    let plen = write_nul_path(sim, limits.small(0), 0, "/t3_20.log");
    sim.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    sim.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    sim.submit(Token::new(3), open_rw_create(limits.small(0), plen))
        .unwrap();
    let cs = drive_until(sim, out, 1000, |v| {
        [1, 2, 3]
            .iter()
            .all(|t| v.iter().any(|c| c.token() == Token::new(*t)))
    })
    .unwrap();
    for c in &cs {
        log.push((c.token().get(), c.result(), c.more()));
    }
    Sock::new(u32::try_from(cs.iter().find(|c| c.token().get() == 1).unwrap().result()).unwrap())
}

/// Sends 1 KiB to `srv` (armed recv) while writing the same buffer to the
/// file; logs every completion and gives ring buffers back.
#[cfg(feature = "sim")]
fn send_and_write_logged(
    sim: &mut SimIo,
    out: &mut Completions,
    srv: Sock,
    log: &mut Vec<(u64, i32, bool)>,
) {
    sim.submit(Token::new(4), Op::ArmedRecv { sock: srv })
        .unwrap();
    write_pool_bytes(sim, BufId::new(0), 0, &[0x33; 1024]);
    sim.submit(
        Token::new(5),
        Op::Send {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: 1024,
            zero_copy: false,
        },
    )
    .unwrap();
    sim.submit(
        Token::new(6),
        Op::FileWrite {
            slot: FileSlot::new(0),
            buf: BufId::new(0),
            off: 0,
            len: 1024,
            file_off: 0,
        },
    )
    .unwrap();
    let cs2 = drive_until(sim, out, 1000, |v| {
        v.iter().any(|c| c.token().get() == 4)
            && v.iter().any(|c| c.token().get() == 5)
            && v.iter().any(|c| c.token().get() == 6)
    })
    .unwrap();
    for c in &cs2 {
        log.push((c.token().get(), c.result(), c.more()));
        if let Some(id) = c.buf() {
            sim.give_back(id);
        }
    }
}

/// One dual-fault run: returns the trace hash and the completion log.
#[cfg(feature = "sim")]
fn dual_fault_run(seed: u64) -> (u64, Vec<(u64, i32, bool)>) {
    let mut limits = net_limits(10, 5, 4, 4, 1024);
    limits = limits.with_files_max(2);
    let mut sim = dual_fault_sim(limits, seed);
    let mut out = Completions::new(32);
    let mut log = Vec::new();
    let srv = connect_and_open_logged(&mut sim, &mut out, limits, &mut log);
    send_and_write_logged(&mut sim, &mut out, srv, &mut log);
    (sim.trace_hash(), log)
}

#[cfg(feature = "sim")]
#[test]
fn t3_20_combined_network_and_disk_faults_deterministic_replay() {
    let (h1, l1) = dual_fault_run(320);
    let (h2, l2) = dual_fault_run(320);
    assert_eq!(h1, h2);
    assert_eq!(l1, l2);
}
