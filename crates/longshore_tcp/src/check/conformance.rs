// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What `Net` relies on from any transport, checked on the transport
//! itself, with no `Net` above it:
//!
//! - every operation that was started is reported once, as ended;
//! - a close is reported once, and the operations it ends are reported
//!   too, before or after it, and then nothing more for the slot;
//! - a close of a slot that holds no socket is reported like any other;
//! - a connect whose slot is closed before it ends leaves no socket: the
//!   slot can connect again;
//! - after the end of the peer's stream is reported, sends still work and
//!   are reported, until the write side is shut down (half-close);
//! - an operation the transport refuses for want of room in its driver
//!   has changed nothing: nothing is reported for it, nothing was sent,
//!   and the same operation works when there is room again.
//!
//! A transport brings a [`Rig`]: itself, its driver, and a way to turn
//! the driver. [`run_all`] is the kernel transport's own.

mod kernel;
mod refusal;

use super::{fail, must};
use crate::transport::{Happened, Listening, Span, Transport};
use crate::Addr;
use longshore_common::io::{Io, Limits};
use std::net::SocketAddr;

pub use kernel::KernelRig;

/// A transport with one listener slot (0), two connection slots for
/// connects (0 and 1), and at least two that accepts fill.
pub trait Rig {
    /// The transport checked.
    type T: Transport;
    /// The transport and its driver.
    fn parts(&mut self) -> (&mut Self::T, &mut <Self::T as Transport>::Driver);
    /// Turn the driver once; what the transport reports goes to `seen`.
    fn turn(&mut self, seen: &mut Vec<Happened>);
    /// Pool memory number `i` (up to 7), `len` bytes of it.
    fn span(&self, i: u32, len: u32) -> Span;
    /// Where the listener listens.
    fn addr(&self) -> Addr;
    /// Take all the room the driver has for operations, so that the
    /// transport must refuse the next one; `false` when this rig cannot.
    fn starve(&mut self) -> bool;
    /// Turn until the room [`Rig::starve`] took is back.
    fn relieve(&mut self, seen: &mut Vec<Happened>);
}

/// Do `f` with the transport and its driver.
pub(super) fn with<R: Rig, V>(
    rig: &mut R,
    f: impl FnOnce(&mut R::T, &mut <R::T as Transport>::Driver) -> V,
) -> V {
    let (t, d) = rig.parts();
    f(t, d)
}

/// Turn until `done` says so of everything reported so far.
pub(super) fn until<R: Rig>(
    rig: &mut R,
    seen: &mut Vec<Happened>,
    what: &str,
    done: impl Fn(&[Happened]) -> bool,
) {
    for _ in 0..20_000 {
        if done(seen) {
            return;
        }
        rig.turn(seen);
    }
    fail(format_args!("{what}: never reported; reported: {seen:?}"));
}

/// Turn a while longer, for anything that should not be reported.
pub(super) fn settle<R: Rig>(rig: &mut R, seen: &mut Vec<Happened>) {
    for _ in 0..200 {
        rig.turn(seen);
    }
}

pub(super) fn count(seen: &[Happened], of: impl Fn(&Happened) -> bool) -> usize {
    seen.iter().filter(|h| of(h)).count()
}

/// Reports that name connection slot `slot`.
pub(super) fn of_slot(h: &Happened, slot: u32) -> bool {
    match *h {
        Happened::Connected { slot: s, .. }
        | Happened::Secured { slot: s, .. }
        | Happened::Accepted { slot: s, .. }
        | Happened::RingData { slot: s, .. }
        | Happened::RecvEnded { slot: s, .. }
        | Happened::Received { slot: s, .. }
        | Happened::Sent { slot: s, .. }
        | Happened::ShutDown { slot: s, .. }
        | Happened::Closed { slot: s } => s == slot,
        Happened::AcceptFailed { slot: s, .. } => s == Some(slot),
        Happened::Nothing | Happened::ListenerClosed { .. } => false,
    }
}

/// Listen, accept, and connect slot 0: the slot the accept filled.
pub(super) fn pair<R: Rig>(rig: &mut R, seen: &mut Vec<Happened>) -> u32 {
    let (addr, scratch) = (rig.addr(), rig.span(7, 256));
    with(rig, |t, d| {
        let bound = must(t.listen(d, 0, &addr, Listening::new(false, 16)), "listen");
        must(t.accept_armed(d, 0), "accept");
        must(
            t.connect(d, 0, &bound.map_or(addr, Addr::Ip), scratch),
            "connect",
        );
    });
    let accepted = |h: &Happened| matches!(h, Happened::Accepted { .. });
    let connected =
        |h: &Happened| matches!(h, Happened::Connected { slot: 0, result } if *result >= 0);
    until(rig, seen, "the connection", |s| {
        count(s, accepted) == 1 && count(s, connected) == 1
    });
    match seen.iter().find(|h| accepted(h)) {
        Some(Happened::Accepted { slot, .. }) => *slot,
        _ => fail(format_args!("no accepted slot")),
    }
}

/// A receive on `slot` into `into`: how it ended. A receive that ends at
/// once must not be reported as well.
pub(super) fn receive<R: Rig>(rig: &mut R, seen: &mut Vec<Happened>, slot: u32, into: Span) -> i32 {
    let received = |h: &Happened| matches!(h, Happened::Received { slot: s, .. } if *s == slot);
    let before = count(seen, received);
    let at_once = with(rig, |t, d| must(t.recv_into(d, slot, into), "recv_into"));
    if let Some(result) = at_once {
        settle(rig, seen);
        if count(seen, received) != before {
            fail(format_args!(
                "a receive that ended at once was reported too"
            ));
        }
        return result;
    }
    until(rig, seen, "a receive", |s| count(s, received) == before + 1);
    match seen.iter().rev().find(|h| received(h)) {
        Some(Happened::Received { result, .. }) => *result,
        _ => fail(format_args!("no receive")),
    }
}

/// Bytes sent are reported once and arrive; a close under a receive is
/// reported once, the receive once, and then nothing more for the slot.
fn every_operation_is_reported_once<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let (from, into) = (rig.span(0, 100), rig.span(1, 4096));
    with(rig, |t, d| {
        t.bytes(d, from.buf())[..100].fill(0x5a);
        must(t.send(d, 0, from, false), "send");
    });
    let sent = |h: &Happened| {
        matches!(
            h,
            Happened::Sent {
                slot: 0,
                result: 100
            }
        )
    };
    until(rig, &mut seen, "the send", |all| count(all, sent) == 1);
    if receive(rig, &mut seen, s, into) != 100 {
        fail(format_args!("the bytes did not arrive whole: {seen:?}"));
    }
    // A receive is waiting when the slot is closed under it.
    let reported = count(
        &seen,
        |h| matches!(h, Happened::Received { slot, .. } if *slot == s),
    );
    let waiting = with(rig, |t, d| {
        let waiting = must(t.recv_into(d, s, into), "recv_into");
        must(t.close(d, s), "close");
        waiting
    });
    let closed = |h: &Happened| matches!(h, Happened::Closed { slot } if *slot == s);
    let ended = |h: &Happened| matches!(h, Happened::Received { slot, .. } if *slot == s);
    // The receive is owed a report unless it ended at once; the one
    // before it was reported only if it did not.
    let owed = reported + usize::from(waiting.is_none());
    until(rig, &mut seen, "the close and what it ended", |all| {
        count(all, closed) == 1 && count(all, ended) == owed
    });
    let so_far = count(&seen, |h| of_slot(h, s));
    settle(rig, &mut seen);
    if count(&seen, |h| of_slot(h, s)) != so_far || count(&seen, sent) != 1 {
        fail(format_args!("reported again after the end: {seen:?}"));
    }
}

/// A close of a slot that never held a socket is reported, once; and a
/// connect whose slot is closed before it ends is reported once too, and
/// leaves the slot able to connect again.
fn closes_of_empty_and_connecting_slots<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    with(rig, |t, d| must(t.close(d, 1), "close of an empty slot"));
    let closed =
        |slot: u32| move |h: &Happened| matches!(h, Happened::Closed { slot: s } if *s == slot);
    until(rig, &mut seen, "the close of an empty slot", |all| {
        count(all, closed(1)) == 1
    });
    let (addr, scratch) = (rig.addr(), rig.span(7, 256));
    let to = with(rig, |t, d| {
        let bound = must(t.listen(d, 0, &addr, Listening::new(false, 16)), "listen");
        must(t.accept_armed(d, 0), "accept");
        let to = bound.map_or(addr, Addr::Ip);
        must(t.connect(d, 0, &to, scratch), "connect");
        must(t.close(d, 0), "close under the connect");
        to
    });
    let connected = |h: &Happened| matches!(h, Happened::Connected { slot: 0, .. });
    until(rig, &mut seen, "the connect and its close", |all| {
        count(all, connected) == 1 && count(all, closed(0)) == 1
    });
    settle(rig, &mut seen);
    // No socket was left in the slot: it connects again. (The first
    // connect may well have succeeded before its close took effect.)
    let up = |h: &Happened| matches!(h, Happened::Connected { slot: 0, result } if *result >= 0);
    let (before, attempts) = (count(&seen, up), count(&seen, connected));
    with(rig, |t, d| {
        must(t.connect(d, 0, &to, scratch), "connect again")
    });
    until(rig, &mut seen, "the second connect", |all| {
        count(all, connected) == attempts + 1
    });
    if count(&seen, up) != before + 1 {
        fail(format_args!("the slot could not connect again: {seen:?}"));
    }
    if count(&seen, closed(1)) != 1 || count(&seen, closed(0)) != 1 {
        fail(format_args!("a close was reported twice: {seen:?}"));
    }
}

/// After the end of the peer's stream is reported, the other direction
/// still carries bytes.
fn sends_go_on_after_the_peers_end<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let (from, into) = (rig.span(0, 300), rig.span(1, 4096));
    with(rig, |t, d| must(t.shutdown_write(d, 0), "shutdown"));
    let shut = |h: &Happened| matches!(h, Happened::ShutDown { slot: 0, result: 0 });
    until(rig, &mut seen, "the shutdown", |all| count(all, shut) == 1);
    if receive(rig, &mut seen, s, into) != 0 {
        fail(format_args!("no end of stream at the peer: {seen:?}"));
    }
    with(rig, |t, d| {
        t.bytes(d, from.buf())[..300].fill(0xa5);
        must(t.send(d, s, from, false), "send after the peer's end");
    });
    let sent = |h: &Happened| matches!(h, Happened::Sent { slot, result: 300 } if *slot == s);
    until(rig, &mut seen, "the send after the peer's end", |all| {
        count(all, sent) == 1
    });
    if receive(rig, &mut seen, 0, into) != 300 {
        fail(format_args!("the answer did not arrive: {seen:?}"));
    }
}

/// Every statement, each on a rig of its own from `rig`.
pub fn check<R: Rig>(rig: &mut impl FnMut() -> R) {
    every_operation_is_reported_once(&mut rig());
    closes_of_empty_and_connecting_slots(&mut rig());
    sends_go_on_after_the_peers_end(&mut rig());
    refusal::a_refused_operation_changes_nothing(&mut rig());
}

/// The kernel transport on a backend from `make`, listening at `addr`
/// (port 0 lets the system choose).
pub fn run_all<IO: Io>(make: &mut impl FnMut(Limits) -> IO, addr: SocketAddr) {
    check(&mut || KernelRig::new(make, addr));
}
