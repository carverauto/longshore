// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared linked-chain contract and zero-allocation tests on [`SimIo`].

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::contract_chains;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[global_allocator]
static ALLOC: Guard = Guard;

fn make_sim(limits: Limits, delay: u32) -> SimIo {
    seeded_sim(limits, 88, delay)
}

fn seeded_sim(limits: Limits, seed: u64, delay: u32) -> SimIo {
    let mut sim = SimIo::new(limits, seed, Faults::NONE.with_delay(delay));
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(32)
            .with_max_dirs(16)
            .with_max_bytes_per_file(64 * 1024)
            .with_max_name_len(256),
    );
    sim
}

#[test]
fn sim_chain_contract_and_zero_alloc() {
    contract_chains::run_all(&mut |l| make_sim(l, 0), "");
    contract_chains::run_all(&mut |l| make_sim(l, 200_000), "");

    let limits = Limits::BASE
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_files_max(2)
        .with_ops_max(8);
    let mut sim = make_sim(limits, 0);
    let mut out = Completions::new(8);
    let p = b"/chain_sealed.dat\0";
    sim.buf(BufId::new(0))[..p.len()].copy_from_slice(p);
    sim.buf(BufId::new(1))[..6].copy_from_slice(b"walrec");

    sealed(|| {
        sim.submit_chain(&[
            (
                Token::new(1),
                Op::FileOpen {
                    slot: FileSlot::new(0),
                    path_buf: BufId::new(0),
                    path_off: 0,
                    path_len: p.len() as u32,
                    flags: OpenFlags::NONE
                        .with_read(true)
                        .with_write(true)
                        .with_create(true),
                },
            ),
            (
                Token::new(2),
                Op::FileWrite {
                    slot: FileSlot::new(0),
                    buf: BufId::new(1),
                    off: 0,
                    len: 6,
                    file_off: 0,
                },
            ),
            (
                Token::new(3),
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: true,
                },
            ),
            (
                Token::new(4),
                Op::FileClose {
                    slot: FileSlot::new(0),
                },
            ),
        ])
        .unwrap();
        let mut done = 0;
        while done < 4 {
            sim.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert!(c.result() >= 0);
                done += 1;
            }
        }
    });
}

const P1: &[u8] = b"/c1.dat\0";
const P2: &[u8] = b"/c2.dat\0";

/// Opens the path in `path_buf` (`path` long) read-write into slot 0.
fn open_rw(path_buf: u32, path: &[u8]) -> Op {
    Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf: BufId::new(path_buf),
        path_off: 0,
        path_len: path.len() as u32,
        flags: OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true),
    }
}

/// Opens `P1` (token 1), cancels it (token 2), and chains a close (token 3)
/// and an open of `P2` (token 4) behind it on the same slot.
#[cfg(test)]
fn cancel_open_then_chain_close_open(sim: &mut SimIo) {
    sim.submit(Token::new(1), open_rw(0, P1)).unwrap();
    sim.submit(
        Token::new(2),
        Op::Cancel {
            target: Token::new(1),
        },
    )
    .unwrap();
    sim.submit_chain(&[
        (
            Token::new(3),
            Op::FileClose {
                slot: FileSlot::new(0),
            },
        ),
        (Token::new(4), open_rw(1, P2)),
    ])
    .unwrap();
}

/// Ticks (10 000 times at most) until tokens 1 to 4 have completed; their results.
#[cfg(test)]
fn results_of_four(sim: &mut SimIo, out: &mut Completions) -> [i32; 4] {
    let mut got = [None; 4];
    for _ in 0..10_000 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if let Some(idx) = [1u64, 2, 3, 4].iter().position(|&t| t == c.token().get()) {
                got[idx] = Some(c.result());
            }
        }
        if got.iter().all(Option::is_some) {
            break;
        }
    }
    got.map(|r| r.expect("all completions delivered"))
}

/// Submits `op` as `tok`, ticks once, and returns the completion's result.
#[cfg(test)]
fn run_one(sim: &mut SimIo, out: &mut Completions, tok: u64, op: Op) -> i32 {
    sim.submit(Token::new(tok), op).unwrap();
    sim.tick(true, out).unwrap();
    out.pop().unwrap().result()
}

#[test]
fn sim_cancelled_open_then_close_open_chain_does_not_deadlock() {
    use longshore_common::io::{EBADF, ECANCELED, ENOENT};

    let mut saw_cancelled_open = false;
    for seed in 0..16u64 {
        let limits = Limits::BASE
            .with_buffers(2)
            .with_buffer_size(256)
            .with_files_max(2)
            .with_ops_max(8);
        let mut sim = seeded_sim(limits, seed, 500_000);
        let mut out = Completions::new(8);
        sim.buf(BufId::new(0))[..P1.len()].copy_from_slice(P1);
        sim.buf(BufId::new(1))[..P2.len()].copy_from_slice(P2);

        cancel_open_then_chain_close_open(&mut sim);
        let res = results_of_four(&mut sim, &mut out);
        if res[1] == 0 {
            saw_cancelled_open = true;
            assert_eq!(res[0], -ECANCELED);
            assert_eq!(res[2], -EBADF);
            assert_eq!(res[3], -ECANCELED);
            assert_eq!(run_one(&mut sim, &mut out, 5, open_rw(1, P2)), 0);
        } else {
            assert_eq!(res[1], -ENOENT);
            assert_eq!(res[0], 0);
            assert_eq!(res[2], 0);
            assert_eq!(res[3], 0);
        }
        let close = Op::FileClose {
            slot: FileSlot::new(0),
        };
        assert_eq!(run_one(&mut sim, &mut out, 6, close), 0);
    }
    assert!(
        saw_cancelled_open,
        "expected at least one seed to cancel FileOpen"
    );
}
