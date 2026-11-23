// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 26 Tier 1 E2E tests: mailbox `sync`, multi-core `SimIo` replay & `test-model.sh`.

#[cfg(feature = "sim")]
use super::super::harness::{drive_until_nonblocking, write_pool_bytes};
#[cfg(feature = "sim")]
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::sync::{AtomicU32, UnsafeCell};
#[cfg(feature = "sim")]
use longshore::mailbox::{queue, TryRecv};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(feature = "sim")]
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;

pub(super) fn workspace_root() -> PathBuf {
    crate::e2e::harness::workspace_root()
}

#[cfg(feature = "sim")]
#[derive(Clone, Copy)]
pub(super) enum CoreTick {
    A,
    B,
}

#[cfg(feature = "sim")]
pub(super) fn run_two_core_ping_pong(
    seed_a: u64,
    seed_b: u64,
    faults_b: Faults,
    schedule: &[CoreTick],
) -> (Vec<u64>, Vec<u64>, u64, u64) {
    let limits = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut core_a = SimIo::new(limits, seed_a, Faults::default());
    let mut core_b = SimIo::new(limits, seed_b, faults_b);
    let mut out_a = Completions::new(16);
    let mut out_b = Completions::new(16);

    let (tx_ab, mut m_ab) = queue::<u64, _>(&mut core_b, 16, Token(1)).expect("b io rx");
    let (tx_ba, mut m_ba) = queue::<u64, _>(&mut core_a, 16, Token(2)).expect("a io rx");
    m_ab.arm(&mut core_b).unwrap();
    m_ba.arm(&mut core_a).unwrap();

    let addr: SocketAddr = "192.0.2.26:9600".parse().unwrap();
    let _: SocketAddr = core_b.listen(Sock(0), addr).unwrap();
    core_b
        .submit(
            Token(50),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
    core_b
        .submit(
            Token(51),
            Op::Connect {
                sock: Sock(1),
                addr,
            },
        )
        .unwrap();
    drive_until_nonblocking(&mut core_b, &mut out_b, 20, |v| v.len() == 2)
        .expect("accept and connect complete");
    write_pool_bytes(&mut core_b, BufId(0), 0, b"core-b-net");
    core_b
        .submit(
            Token(52),
            Op::Send {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 10,
                zero_copy: false,
            },
        )
        .unwrap();

    let mut b_seen = Vec::new();
    let mut a_seen = Vec::new();
    let mut next_ping = 1u64;
    tx_ab.try_send(next_ping).unwrap();

    for turn in schedule {
        match turn {
            CoreTick::A => {
                core_a.tick(false, &mut out_a).unwrap();
                while let Some(c) = out_a.pop() {
                    if m_ba.on_completion(&mut core_a, c).unwrap() {
                        while let TryRecv::Message(ans) = m_ba.try_recv() {
                            a_seen.push(ans);
                            if next_ping < 50 {
                                next_ping += 1;
                                tx_ab
                                    .try_send(next_ping)
                                    .expect("one ping in flight fits the queue");
                            }
                        }
                    }
                }
            }
            CoreTick::B => {
                core_b.tick(false, &mut out_b).unwrap();
                while let Some(c) = out_b.pop() {
                    if m_ab.on_completion(&mut core_b, c).unwrap() {
                        while let TryRecv::Message(req) = m_ab.try_recv() {
                            b_seen.push(req);
                            tx_ba
                                .try_send(req * 10)
                                .expect("one answer in flight fits the queue");
                        }
                    }
                }
            }
        }
        if a_seen.len() >= 50 {
            break;
        }
    }
    (b_seen, a_seen, core_a.trace_hash(), core_b.trace_hash())
}

#[cfg(feature = "sim")]
#[test]
fn t1_f26_1_two_sim_backends_exchange_50_ping_pong_requests_nonblocking() {
    let sched: Vec<CoreTick> = (0..300)
        .map(|i| if i % 2 == 0 { CoreTick::B } else { CoreTick::A })
        .collect();
    let (b_seen, a_seen, _, _) = run_two_core_ping_pong(11, 22, Faults::default(), &sched);
    assert_eq!(b_seen.len(), 50);
    assert_eq!(a_seen.len(), 50);
    assert_eq!(a_seen.last().copied(), Some(500));
}

#[cfg(feature = "sim")]
#[test]
fn t1_f26_2_identical_seeds_and_schedule_produce_identical_traces() {
    let sched: Vec<CoreTick> = (0..300)
        .map(|i| if i % 2 == 0 { CoreTick::B } else { CoreTick::A })
        .collect();
    let r1 = run_two_core_ping_pong(11, 22, Faults::default(), &sched);
    let r2 = run_two_core_ping_pong(11, 22, Faults::default(), &sched);
    assert_eq!(r1, r2);
}

#[cfg(feature = "sim")]
#[test]
fn t1_f26_3_distinct_tick_schedules_produce_deterministic_order_differences() {
    let limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    };
    let run_sched = |a_first: bool| -> Vec<u64> {
        let mut core_a = SimIo::new(limits, 11, Faults::default());
        let mut core_b = SimIo::new(limits, 22, Faults::default());
        let mut out_a = Completions::new(8);
        let mut out_b = Completions::new(8);
        let (tx, mut m_rx) = queue::<u64, _>(&mut core_b, 8, Token(1)).unwrap();
        m_rx.arm(&mut core_b).unwrap();

        let (tx_a, tx_b) = (tx.clone(), tx);
        let mut order = Vec::new();
        if a_first {
            tx_a.try_send(1).unwrap();
            core_a.tick(false, &mut out_a).unwrap();
            tx_b.try_send(2).unwrap();
        } else {
            tx_b.try_send(2).unwrap();
            core_b.tick(false, &mut out_b).unwrap();
            tx_a.try_send(1).unwrap();
        }
        core_b.tick(false, &mut out_b).unwrap();
        while let TryRecv::Message(v) = m_rx.try_recv() {
            order.push(v);
        }
        order
    };
    assert_eq!(run_sched(true), vec![1, 2]);
    assert_eq!(run_sched(false), vec![2, 1]);
}

#[test]
fn t1_f26_4_test_model_script_passes_mailbox_and_client_model_suites() {
    let out = Command::new("sh")
        .arg("scripts/test-model.sh")
        .current_dir(workspace_root())
        .output()
        .expect("run scripts/test-model.sh");
    assert!(
        out.status.success(),
        "scripts/test-model.sh failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn t1_f26_5_mailbox_sync_unsafe_cell_and_atomics_work_under_std() {
    let cell = UnsafeCell::new(10u32);
    let flag = AtomicU32::new(0);
    cell.with_mut(|ptr| unsafe {
        *ptr += 5;
    });
    flag.store(1, Ordering::Release);
    assert_eq!(flag.load(Ordering::Acquire), 1);
    let val = cell.with(|ptr| unsafe { *ptr });
    assert_eq!(val, 15);
}
