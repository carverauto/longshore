// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 2: `io::engine` (`Engine<W>`, `World`) (`#41`).

use crate::e2e::harness::{drain_completions, drive_until, HookCall, OpKind, TestWorld};
use longshore::io::engine::{Engine, World};
use longshore::io::{BufId, Completions, FileId, Io, Limits, Op, Sock, Token, Wake, ECANCELED};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct WakeTestWorld {
    inner: TestWorld,
}

struct FlagWaker(Arc<AtomicBool>);

impl Wake for FlagWaker {
    fn wake(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl World for WakeTestWorld {
    fn now_ns(&self) -> u64 {
        self.inner.now_ns()
    }
    fn unix_secs(&self) -> u64 {
        self.inner.unix_secs()
    }
    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.inner.register_file(path)
    }
    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.inner.listen(sock, addr)
    }
    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        self.inner.listen_unix(sock, path)
    }
    fn waker(&mut self, woken: Arc<AtomicBool>) -> Option<Arc<dyn Wake>> {
        Some(Arc::new(FlagWaker(woken)))
    }
    fn is_ready(&self, op: &Op) -> bool {
        self.inner.is_ready(op)
    }
    fn advance(&mut self) {
        self.inner.advance();
    }
    fn first(&mut self, n: usize) -> usize {
        self.inner.first(n)
    }
    fn hold(&mut self, op: &Op) -> bool {
        self.inner.hold(op)
    }
    fn attempt(&mut self, op: &Op, pool: &mut [u8], limits: &Limits) -> Option<i32> {
        self.inner.attempt(op, pool, limits)
    }
    fn claimed(&self, op: &Op) -> bool {
        self.inner.claimed(op)
    }
    fn release(&mut self, op: &Op) {
        self.inner.release(op);
    }
    fn closed(&mut self, sock: Sock) {
        self.inner.closed(sock);
    }
    fn wait(&mut self, deadline: Option<u64>, only_timers: bool, idle: u32) -> io::Result<()> {
        self.inner.wait(deadline, only_timers, idle)
    }
}

pub(super) fn base_limits() -> Limits {
    Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 128,
        ops_max: 6,
        ..Limits::BASE
    }
}

#[test]
fn t1_f02_1_world_hooks_timeout_sequence() {
    let mut engine = Engine::with_world(base_limits(), TestWorld::new());
    let mut out = Completions::new(8);
    engine
        .submit(Token(1), Op::Timeout { nanos: 200_000 })
        .expect("submit");
    let cs = drive_until(&mut engine, &mut out, 10, |v| !v.is_empty()).expect("tick");
    assert_eq!(cs.len(), 1);
    assert_eq!((cs[0].token, cs[0].result), (Token(1), 0));

    let calls = engine.world().calls();
    assert_eq!(calls[0], HookCall::Advance);
    assert_eq!(calls[1], HookCall::First(1));
    assert_eq!(calls[2], HookCall::Hold(OpKind::Timeout));
    assert!(
        calls.iter().any(|c| matches!(
            c,
            HookCall::Wait {
                deadline: Some(200_000),
                only_timers: true,
                ..
            }
        )),
        "expected timer wait hook, got: {:?}",
        &*calls
    );
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, HookCall::Attempt(OpKind::Timeout))),
        "Timeout must resolve inside Engine without World::attempt"
    );
}

#[test]
fn t1_f02_2_engine_resolves_await_wake_locally() {
    let world = WakeTestWorld {
        inner: TestWorld::new(),
    };
    let mut engine = Engine::with_world(base_limits(), world);
    let mut out = Completions::new(8);
    let waker = engine.waker().expect("waker");
    waker.wake();
    engine.submit(Token(2), Op::AwaitWake).expect("submit");
    let cs = drive_until(&mut engine, &mut out, 5, |v| !v.is_empty()).expect("tick");
    assert_eq!(cs.len(), 1);
    assert_eq!((cs[0].token, cs[0].result), (Token(2), 0));
    assert!(!engine
        .world()
        .inner
        .calls()
        .iter()
        .any(|c| matches!(c, HookCall::Attempt(OpKind::AwaitWake))));
}

#[test]
fn t1_f02_3_engine_resolves_cancel_locally() {
    let mut engine = Engine::with_world(base_limits(), TestWorld::new());
    let mut out = Completions::new(8);
    engine
        .submit(
            Token(10),
            Op::Recv {
                sock: Sock(0),
                buf: BufId(0),
                off: 0,
                len: 64,
            },
        )
        .expect("submit recv");
    engine.tick(false, &mut out).expect("tick");
    assert!(out.is_empty());
    engine.world().clear_calls();

    engine
        .submit(Token(11), Op::Cancel { target: Token(10) })
        .expect("submit cancel");
    let mut cs = drive_until(&mut engine, &mut out, 5, |v| v.len() == 2).expect("drain");
    cs.sort_by_key(|c| c.token.0);
    assert_eq!(
        cs.iter().map(|c| (c.token.0, c.result)).collect::<Vec<_>>(),
        vec![(10, -ECANCELED), (11, 0)]
    );
    assert!(!engine
        .world()
        .calls()
        .iter()
        .any(|c| matches!(c, HookCall::Attempt(OpKind::Cancel))));
}

#[test]
fn t1_f02_4_engine_respects_world_readiness_filter() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let after_first = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&after_first);
    let world = TestWorld::new()
        .with_ready_socks(vec![Sock(1)])
        .with_attempt_handler(move |op, _, _| match *op {
            Op::Recv { sock: Sock(1), .. } => Some(4),
            Op::Recv { sock: Sock(2), .. } => {
                assert!(
                    !flag.load(Ordering::Acquire),
                    "unready Sock(2) should be skipped"
                );
                None
            }
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::Send { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileOpen { .. }
            | Op::FileClose { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => None,
        });
    let mut engine = Engine::with_world(base_limits(), world);
    let mut out = Completions::new(8);

    // First tick visits fresh Sock(2) once, then clears its `fresh` bit.
    engine
        .submit(
            Token(20),
            Op::Recv {
                sock: Sock(2),
                buf: BufId(0),
                off: 0,
                len: 32,
            },
        )
        .expect("submit s2");
    engine.tick(false, &mut out).expect("tick1");
    assert!(out.is_empty());
    after_first.store(true, Ordering::Release);
    engine.world().clear_calls();

    // Second turn: submit ready Sock(1), Timeout, and Cancel; Sock(2) must not be attempted.
    engine
        .submit(
            Token(21),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 32,
            },
        )
        .expect("submit s1");
    engine
        .submit(Token(22), Op::Timeout { nanos: 0 })
        .expect("submit timer");
    engine
        .submit(Token(23), Op::Cancel { target: Token(999) })
        .expect("submit cancel");
    engine.tick(false, &mut out).expect("tick2");

    let mut got = Vec::new();
    let _: usize = drain_completions(&mut out, &mut got);
    got.sort_by_key(|c| c.token.0);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].token, Token(21));
    assert_eq!(got[0].result, 4);
    assert_eq!(got[1].token, Token(22));
    assert_eq!(got[1].result, 0);
    assert_eq!(got[2].token, Token(23));
    assert!(got[2].result < 0);

    let attempt_count = engine
        .world()
        .calls()
        .iter()
        .filter(|c| matches!(c, HookCall::Attempt(OpKind::Recv)))
        .count();
    assert_eq!(attempt_count, 1, "only ready Sock(1) must be attempted");
}

#[test]
fn t1_f02_5_engine_clock_and_arrival_order() {
    let mut world = TestWorld::new();
    world.now = 5_000_000;
    world.step_ns = 1_000_000;
    world.unix_epoch = 1_700_000_000;
    world.first_index = 1;
    let mut engine = Engine::with_world(base_limits(), world);
    assert_eq!(engine.now_ns(), 5_000_000);
    assert_eq!(engine.unix_secs(), 1_700_000_000);

    let mut out = Completions::new(8);
    engine.submit(Token(1), Op::Timeout { nanos: 0 }).unwrap();
    engine.submit(Token(2), Op::Timeout { nanos: 0 }).unwrap();
    engine.submit(Token(3), Op::Timeout { nanos: 0 }).unwrap();
    engine.tick(false, &mut out).unwrap();

    assert_eq!(engine.now_ns(), 6_000_000);
    let mut got = Vec::new();
    let _: usize = drain_completions(&mut out, &mut got);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].token, Token(2), "walk started at index 1");
}
