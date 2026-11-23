// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 26 Tier 2 E2E tests: backpressure, stop, faults, `alloc_guard` & model check failure.

use super::f26_mailbox_sim_t1::workspace_root;
#[cfg(feature = "sim")]
use super::f26_mailbox_sim_t1::{run_two_core_ping_pong, CoreTick};
#[cfg(feature = "sim")]
use longshore::io::alloc_guard::sealed;
#[cfg(feature = "sim")]
use longshore::io::{Completions, Io, Limits, Token};
#[cfg(feature = "sim")]
use longshore::mailbox::{queue, TryRecv, TrySendError};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::process::Command;

#[cfg(feature = "sim")]
#[test]
fn t2_f26_1_two_sim_cores_capacity_one_backpressure_replays_cleanly() {
    let run = || -> Vec<u32> {
        let limits = Limits::BASE
            .with_sockets_max(2)
            .with_buffers(1)
            .with_buffer_size(256)
            .with_ops_max(4);
        let mut core_b = SimIo::new(limits, 33, Faults::default());
        let mut out_b = Completions::new(8);
        let (tx, mut m_rx) = queue::<u32, _>(&mut core_b, 1, Token::new(1)).unwrap();
        m_rx.arm(&mut core_b).unwrap();

        let mut received = Vec::new();
        let mut pending = Vec::from([1u32, 2, 3, 4, 5]);
        let mut idx = 0;
        for _ in 0..20 {
            if idx < pending.len() {
                match tx.try_send(pending[idx]) {
                    Ok(()) => idx += 1,
                    Err(TrySendError::Full(v)) => pending[idx] = v,
                    Err(TrySendError::Closed(_)) => unreachable!(),
                }
            }
            core_b.tick(false, &mut out_b).unwrap();
            while let Some(c) = out_b.pop() {
                if m_rx.on_completion(&mut core_b, c).unwrap() {
                    while let TryRecv::Message(v) = m_rx.try_recv() {
                        received.push(v);
                    }
                }
            }
            if received.len() == pending.len() {
                break;
            }
        }
        received
    };
    assert_eq!(run(), vec![1, 2, 3, 4, 5]);
    assert_eq!(run(), vec![1, 2, 3, 4, 5]);
}

#[cfg(feature = "sim")]
#[test]
fn t2_f26_2_two_sim_cores_stop_drains_queued_then_rejects_further_sends() {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(256)
        .with_ops_max(4);
    let mut core_b = SimIo::new(limits, 44, Faults::default());
    let mut out_b = Completions::new(8);
    let (tx, mut m_rx) = queue::<u32, _>(&mut core_b, 5, Token::new(1)).unwrap();
    m_rx.arm(&mut core_b).unwrap();

    for i in 1..=5u32 {
        tx.try_send(i).unwrap();
    }
    tx.stop();
    assert_eq!(tx.try_send(6), Err(TrySendError::Closed(6)));

    core_b.tick(false, &mut out_b).unwrap();
    let mut events = Vec::new();
    loop {
        let ev = m_rx.try_recv();
        if matches!(ev, TryRecv::Empty | TryRecv::Closed) {
            break;
        }
        events.push(ev);
    }
    assert_eq!(
        events,
        vec![
            TryRecv::Message(1),
            TryRecv::Message(2),
            TryRecv::Message(3),
            TryRecv::Message(4),
            TryRecv::Message(5),
            TryRecv::Stop,
        ]
    );
}

#[cfg(feature = "sim")]
#[test]
fn t2_f26_3_two_sim_cores_with_network_faults_replay_deterministically() {
    let sched: Vec<CoreTick> = (0..300)
        .map(|i| if i % 2 == 0 { CoreTick::B } else { CoreTick::A })
        .collect();
    let faults = Faults::NONE.with_delay(200_000).with_short_send(200_000);
    let r1 = run_two_core_ping_pong(101, 202, faults, &sched);
    let r2 = run_two_core_ping_pong(101, 202, faults, &sched);
    assert_eq!(r1, r2);
}

#[cfg(feature = "sim")]
#[test]
fn t2_f26_4_two_sim_cores_steady_state_loop_runs_under_alloc_guard() {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(256)
        .with_ops_max(4);
    let mut core_a = SimIo::new(limits, 51, Faults::default());
    let mut core_b = SimIo::new(limits, 52, Faults::default());
    let mut out_a = Completions::new(8);
    let mut out_b = Completions::new(8);
    let (tx_ab, mut m_ab) = queue::<u32, _>(&mut core_b, 4, Token::new(1)).unwrap();
    let (tx_ba, mut m_ba) = queue::<u32, _>(&mut core_a, 4, Token::new(2)).unwrap();
    m_ab.arm(&mut core_b).unwrap();
    m_ba.arm(&mut core_a).unwrap();

    let total = sealed(|| {
        let mut count = 0u32;
        tx_ab.try_send(1).unwrap();
        for _ in 0..20 {
            core_b.tick(false, &mut out_b).unwrap();
            while let Some(c) = out_b.pop() {
                if m_ab.on_completion(&mut core_b, c).unwrap() {
                    while let TryRecv::Message(v) = m_ab.try_recv() {
                        tx_ba
                            .try_send(v + 1)
                            .expect("one message in flight fits the queue");
                    }
                }
            }
            core_a.tick(false, &mut out_a).unwrap();
            while let Some(c) = out_a.pop() {
                if m_ba.on_completion(&mut core_a, c).unwrap() {
                    while let TryRecv::Message(v) = m_ba.try_recv() {
                        count += v;
                        tx_ab
                            .try_send(v + 1)
                            .expect("one message in flight fits the queue");
                    }
                }
            }
        }
        count
    });
    assert!(total > 0);
}

#[test]
fn t2_f26_5_test_model_script_covers_mailbox_and_fails_on_bad_flags() {
    let script = std::fs::read_to_string(workspace_root().join("scripts/test-model.sh"))
        .expect("read test-model.sh");
    assert!(script.contains("set -eu"));
    assert!(
        script.contains("longshore_mailbox"),
        "scripts/test-model.sh must run longshore_mailbox model tests"
    );
    let out = Command::new("sh")
        .arg("scripts/test-model.sh")
        .env("RUSTFLAGS", "--invalid-rustc-flag-for-test")
        .current_dir(workspace_root())
        .output()
        .expect("invoke test-model.sh");
    assert!(!out.status.success(), "broken RUSTFLAGS must fail non-zero");
}
