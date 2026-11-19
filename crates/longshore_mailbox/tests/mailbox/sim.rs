// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Deterministic multi-core simulation: two `Engine<Virtual>` (`SimIo`)
//! instances on one thread exchanging messages via mailboxes using
//! `tick(false, &mut out)` with zero timers.

#![cfg(not(longshore_model))]

use longshore_common::io::{Completions, Io, Limits, Token};
use longshore_common::sim::{Faults, SimIo};
use longshore_mailbox::{queue, QueueReceiver, QueueSender, Recv};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Msg {
    from: u8,
    seq: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct RunResult {
    delivered_a: Vec<Msg>,
    delivered_b: Vec<Msg>,
    now_a: u64,
    now_b: u64,
    hash_a: u64,
    hash_b: u64,
}

fn run_two_cores(seed_a: u64, seed_b: u64, tick_order: &[usize], faults: Faults) -> RunResult {
    let limits = Limits {
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io_a = SimIo::new(limits, seed_a, faults);
    let mut io_b = SimIo::new(limits, seed_b, faults);

    let (tx_a, mut rx_a) = queue::<Msg, _>(&mut io_a, 8, Token(1)).unwrap();
    let (tx_b, mut rx_b) = queue::<Msg, _>(&mut io_b, 8, Token(2)).unwrap();
    rx_a.arm(&mut io_a).unwrap();
    rx_b.arm(&mut io_b).unwrap();

    // Core A starts with 3 initial messages to Core B, and Core B replies
    // whenever it receives a message from Core A.
    for seq in 0..3 {
        tx_b.try_send(Msg { from: 0, seq }).unwrap();
    }
    tx_a.try_send(Msg { from: 1, seq: 100 }).unwrap();

    let mut out = Completions::new(16);

    struct Core {
        id: u8,
        io: SimIo,
        rx: QueueReceiver<Msg>,
        delivered: Vec<Msg>,
        next_seq: u32,
    }

    fn step_core(core: &mut Core, peer: &QueueSender<Msg>, out: &mut Completions, reply: bool) {
        core.io.tick(false, out).unwrap();
        while let Some(c) = out.pop() {
            assert!(core.rx.on_completion(&mut core.io, c).unwrap());
        }
        core.rx.drain(|item| {
            if let Recv::Message(msg) = item {
                core.delivered.push(msg);
                if reply {
                    peer.try_send(Msg {
                        from: core.id,
                        seq: core.next_seq,
                    })
                    .unwrap();
                    core.next_seq += 1;
                }
            }
        });
    }

    let mut a = Core {
        id: 0,
        io: io_a,
        rx: rx_a,
        delivered: Vec::new(),
        next_seq: 0,
    };
    let mut b = Core {
        id: 1,
        io: io_b,
        rx: rx_b,
        delivered: Vec::new(),
        next_seq: 0,
    };

    for &core in tick_order {
        if core == 0 {
            step_core(&mut a, &tx_b, &mut out, false);
        } else {
            step_core(&mut b, &tx_a, &mut out, true);
        }
    }

    RunResult {
        delivered_a: a.delivered,
        delivered_b: b.delivered,
        now_a: a.io.now_ns(),
        now_b: b.io.now_ns(),
        hash_a: a.io.trace_hash(),
        hash_b: b.io.trace_hash(),
    }
}

#[test]
fn two_virtual_engines_replay_identically_from_seeds_and_tick_order() {
    let order1 = [0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 1, 0];
    let order2 = [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0];
    let faults = Faults {
        delay: 250_000,
        ..Faults::default()
    };

    let r1a = run_two_cores(111, 222, &order1, faults);
    let r1b = run_two_cores(111, 222, &order1, faults);
    assert_eq!(r1a, r1b);

    // Every sent message is delivered in per-sender order.
    assert_eq!(
        r1a.delivered_b,
        vec![
            Msg { from: 0, seq: 0 },
            Msg { from: 0, seq: 1 },
            Msg { from: 0, seq: 2 },
        ]
    );
    assert_eq!(
        r1a.delivered_a,
        vec![
            Msg { from: 1, seq: 100 },
            Msg { from: 1, seq: 0 },
            Msg { from: 1, seq: 1 },
            Msg { from: 1, seq: 2 },
        ]
    );

    // Changing tick order alters the trace / delivery timing.
    let r2 = run_two_cores(111, 222, &order2, faults);
    assert_ne!(
        (r1a.now_a, r1a.now_b, r1a.hash_a, r1a.hash_b),
        (r2.now_a, r2.now_b, r2.hash_a, r2.hash_b)
    );
}
