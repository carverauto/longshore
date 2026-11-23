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

#[cfg(feature = "sim")]
#[test]
fn t3_18_armed_recv_enobufs_under_sim_faults_and_give_back() {
    let limits = net_limits(8, 4, 2, 2, 64);
    let faults = Faults {
        partial_recv: 300_000,
        reset: 50_000,
        ..Default::default()
    };
    let mut sim = SimIo::new(limits, 318, faults);
    let mut out = Completions::new(16);
    let addr = sim
        .listen(Sock(0), "192.0.2.38:9318".parse().unwrap())
        .unwrap();
    sim.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    sim.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();

    sim.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    let payload: Vec<u8> = (0..180u8).collect();
    write_pool_bytes(&mut sim, BufId(0), 0, &payload);
    sim.submit(
        Token(4),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: payload.len() as u32,
            zero_copy: false,
        },
    )
    .unwrap();

    let mut received = Vec::new();
    let mut held = Vec::new();
    let mut ended_with = None;
    for _ in 0..64 {
        sim.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(3) {
                if c.result > 0 {
                    let id = c.buf.unwrap();
                    received.extend_from_slice(&read_pool_bytes(
                        &mut sim,
                        id,
                        0,
                        usize::try_from(c.result).unwrap(),
                    ));
                    held.push(id);
                }
                if !c.more {
                    ended_with = Some(c.result);
                }
            }
        }
        if ended_with.is_some() {
            break;
        }
    }
    let end = ended_with.expect("armed recv must end on -ENOBUFS or -ECONNRESET");
    assert!(end == -ENOBUFS || end == -ECONNRESET || end >= 0);
    if end == -ENOBUFS {
        for id in held.drain(..) {
            sim.give_back(id);
        }
        sim.submit(Token(5), Op::ArmedRecv { sock: Sock(2) })
            .unwrap();
        for _ in 0..64 {
            sim.tick(false, &mut out).unwrap();
            let mut hit_reset = false;
            while let Some(c) = out.pop() {
                if c.token == Token(5) {
                    if c.result > 0 {
                        let id = c.buf.unwrap();
                        received.extend_from_slice(&read_pool_bytes(
                            &mut sim,
                            id,
                            0,
                            usize::try_from(c.result).unwrap(),
                        ));
                        sim.give_back(id);
                    }
                    if !c.more {
                        assert!(c.result == -ECONNRESET || c.result == -ENOBUFS || c.result >= 0);
                        hit_reset = true;
                    }
                }
            }
            if received.len() == payload.len() || hit_reset {
                break;
            }
        }
    }
    assert_eq!(received, payload[..received.len()]);
}

#[cfg(feature = "sim")]
#[test]
fn t3_19_simio_power_loss_resets_armed_ops_and_stream_rebuilds() {
    let mut limits = net_limits(12, 6, 4, 4, 1024);
    limits.files_max = 4;
    let mut sim = SimIo::new(limits, 319, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 4,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: 32 * 1024,
        read_error_sectors: 4,
        sector_size: 512,
    });
    let mut out = Completions::new(16);
    let addr = sim
        .listen(Sock(0), "192.0.2.38:9318".parse().unwrap())
        .unwrap();
    sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    sim.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();
    let srv = Sock(u32::try_from(cs.iter().find(|c| c.token == Token(1)).unwrap().result).unwrap());
    sim.submit(Token(3), Op::ArmedRecv { sock: srv }).unwrap();

    let plen = write_nul_path(&mut sim, limits.small(0), 0, "/t3_19.dat");
    sim.submit(
        Token(4),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: limits.small(0),
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

    sim.power_loss();
    sim.tick(false, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "power_loss drops armed ops and file ops without completions"
    );

    let addr2 = sim
        .listen(Sock(0), "192.0.2.39:9320".parse().unwrap())
        .unwrap();
    sim.submit(Token(10), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    sim.submit(
        Token(11),
        Op::Connect {
            sock: Sock(1),
            addr: addr2,
        },
    )
    .unwrap();
    let cs2 = drive_until(&mut sim, &mut out, 1000, |v| v.len() == 2).unwrap();
    let srv2 =
        Sock(u32::try_from(cs2.iter().find(|c| c.token == Token(10)).unwrap().result).unwrap());
    let mut s = Stream::new(Sock(1), Token(12), Token(13));
    write_pool_bytes(&mut sim, BufId(0), 0, b"rebuilt");
    s.send(&mut sim, BufId(0), 0, 7, u32::MAX);
    sim.submit(Token(14), Op::ArmedRecv { sock: srv2 }).unwrap();
    let cs3 = drive_until(&mut sim, &mut out, 1000, |v| {
        v.iter().any(|c| c.token == Token(14))
    })
    .unwrap();
    let r = cs3.iter().find(|c| c.token == Token(14)).unwrap();
    assert_eq!(
        read_pool_bytes(
            &mut sim,
            r.buf.unwrap(),
            0,
            usize::try_from(r.result).unwrap()
        ),
        b"rebuilt"
    );
    sim.give_back(r.buf.unwrap());
}

#[cfg(feature = "sim")]
#[test]
fn t3_20_combined_network_and_disk_faults_deterministic_replay() {
    fn run_once(seed: u64) -> (u64, Vec<(u64, i32, bool)>) {
        let mut limits = net_limits(10, 5, 4, 4, 1024);
        limits.files_max = 2;
        let net_f = Faults {
            short_send: 200_000,
            partial_recv: 200_000,
            delay: 200_000,
            ..Default::default()
        };
        let disk_f = DiskFaults {
            short_write: 200_000,
            torn_reported: 100_000,
            bit_flip: 100_000,
            ..Default::default()
        };
        let mut sim = SimIo::new(limits, seed, net_f);
        sim.setup_disk(DiskConfig {
            max_files: 4,
            max_dirs: 4,
            max_bytes_per_file: 64 * 1024,
            max_name_len: 128,
            write_log_entries: 16,
            write_log_bytes: 32 * 1024,
            read_error_sectors: 4,
            sector_size: 512,
        });
        sim.set_disk_faults(disk_f);
        let mut out = Completions::new(32);
        let mut log = Vec::new();

        let addr = sim
            .listen(Sock(0), "192.0.2.40:9321".parse().unwrap())
            .unwrap();
        let plen = write_nul_path(&mut sim, limits.small(0), 0, "/t3_20.log");
        sim.submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .unwrap();
        sim.submit(
            Token(2),
            Op::Connect {
                sock: Sock(1),
                addr,
            },
        )
        .unwrap();
        sim.submit(
            Token(3),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: limits.small(0),
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
        let cs = drive_until(&mut sim, &mut out, 1000, |v| {
            [1, 2, 3]
                .iter()
                .all(|t| v.iter().any(|c| c.token == Token(*t)))
        })
        .unwrap();
        for c in &cs {
            log.push((c.token.0, c.result, c.more));
        }
        let srv =
            Sock(u32::try_from(cs.iter().find(|c| c.token == Token(1)).unwrap().result).unwrap());
        sim.submit(Token(4), Op::ArmedRecv { sock: srv }).unwrap();
        write_pool_bytes(&mut sim, BufId(0), 0, &[0x33; 1024]);
        sim.submit(
            Token(5),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 1024,
                zero_copy: false,
            },
        )
        .unwrap();
        sim.submit(
            Token(6),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 1024,
                file_off: 0,
            },
        )
        .unwrap();
        let cs2 = drive_until(&mut sim, &mut out, 1000, |v| {
            v.iter().any(|c| c.token == Token(4))
                && v.iter().any(|c| c.token == Token(5))
                && v.iter().any(|c| c.token == Token(6))
        })
        .unwrap();
        for c in &cs2 {
            log.push((c.token.0, c.result, c.more));
            if let Some(id) = c.buf {
                sim.give_back(id);
            }
        }
        (sim.trace_hash(), log)
    }

    let (h1, l1) = run_once(320);
    let (h2, l2) = run_once(320);
    assert_eq!(h1, h2);
    assert_eq!(l1, l2);
}
