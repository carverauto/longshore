// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! An operation refused for want of room in the driver.

use super::{count, of_slot, pair, receive, settle, until, with, Rig};
use crate::check::{fail, must};
use crate::transport::{Happened, Refused, Transport};

/// With no room in the driver a send, a receive, a shutdown and a close
/// are refused, and have changed nothing: nothing is reported for them,
/// nothing was sent, and each works once there is room again.
pub(super) fn a_refused_operation_changes_nothing<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let (from, into) = (rig.span(0, 100), rig.span(1, 4096));
    if !rig.starve() {
        return;
    }
    let refused = with(rig, |t, d| {
        t.bytes(d, from.buf())[..100].fill(0x3c);
        [
            t.send(d, 0, from, false),
            t.recv_into(d, s, into).map(|_| ()),
            t.shutdown_write(d, 0),
            t.close(d, s),
        ]
    });
    if refused != [Err(Refused::Busy); 4] {
        fail(format_args!("not refused as busy: {refused:?}"));
    }
    let ours = |all: &[Happened]| count(all, |h| of_slot(h, 0) || of_slot(h, s));
    let before = ours(&seen);
    rig.relieve(&mut seen);
    settle(rig, &mut seen);
    if ours(&seen) != before {
        fail(format_args!("a refused operation was reported: {seen:?}"));
    }
    // Each works now, once. The receive gets one send's bytes: the
    // refused send sent none.
    with(rig, |t, d| must(t.send(d, 0, from, false), "send"));
    let sent = |h: &Happened| matches!(h, Happened::Sent { slot: 0, .. });
    until(rig, &mut seen, "the send", |all| count(all, sent) == 1);
    if receive(rig, &mut seen, s, into) != 100 {
        fail(format_args!("not one send's bytes: {seen:?}"));
    }
    with(rig, |t, d| must(t.shutdown_write(d, 0), "shutdown"));
    let shut = |h: &Happened| matches!(h, Happened::ShutDown { slot: 0, .. });
    until(rig, &mut seen, "the shutdown", |all| count(all, shut) == 1);
    if receive(rig, &mut seen, s, into) != 0 {
        fail(format_args!("no end of stream: {seen:?}"));
    }
    with(rig, |t, d| must(t.close(d, s), "close"));
    let closed = |h: &Happened| matches!(h, Happened::Closed { slot } if *slot == s);
    until(rig, &mut seen, "the close", |all| count(all, closed) == 1);
    settle(rig, &mut seen);
    if count(&seen, sent) != 1 || count(&seen, shut) != 1 || count(&seen, closed) != 1 {
        fail(format_args!("reported more than once: {seen:?}"));
    }
}
