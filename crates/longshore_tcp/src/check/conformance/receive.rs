// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The receives `Net` falls back to when the buffer ring has nothing for
//! a connection: a single receive after an armed one has ended, and on a
//! protected slot a receive into the transport's own memory.

use super::{count, of_slot, pair, receive, settle, until, with, Rig};
use crate::check::{fail, must};
use crate::transport::{Happened, Refused, Role, Span, Transport};

/// An armed receive is stopped, and reports its end once; a single
/// receive on the slot then delivers what the peer sends, once.
pub(super) fn a_single_receive_follows_an_armed_one<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let (from, into) = (rig.span(0, 100), rig.span(1, 4096));
    with(rig, |t, d| {
        must(t.recv_armed(d, s), "recv_armed");
        must(t.cancel_recv(d, s), "cancel_recv");
    });
    let ended = |h: &Happened| matches!(h, Happened::RecvEnded { slot, .. } if *slot == s);
    until(rig, &mut seen, "the end of the armed receive", |all| {
        count(all, ended) == 1
    });
    with(rig, |t, d| {
        t.bytes(d, from.buf())[..100].fill(0x71);
        must(t.send(d, 0, from, false), "send");
    });
    if receive(rig, &mut seen, s, into) != 100 {
        fail(format_args!("the single receive got other bytes: {seen:?}"));
    }
    let so_far = count(&seen, |h| of_slot(h, s));
    settle(rig, &mut seen);
    if count(&seen, |h| of_slot(h, s)) != so_far || count(&seen, ended) != 1 {
        fail(format_args!("reported again: {seen:?}"));
    }
}

/// On a protected slot a receive into the transport's own memory is
/// reported once, what it brought is opened with no bytes, and one that a
/// close ends is reported once too. A transport without such memory
/// refuses as unsupported, and that is all there is to hold it to.
pub(super) fn a_kept_receive_is_reported_once<R: Rig>(rig: &mut R) {
    let Some(name) = rig.server_name() else {
        return;
    };
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let (from, into) = (rig.span(0, 100), rig.span(1, 32 << 10));
    // No bytes of a ring buffer: the transport opens what it holds.
    let nothing = rig.span(2, 0);
    with(rig, |t, d| {
        let client = Role::Client {
            server_name: name.as_str(),
        };
        must(t.secure(d, 0, client), "secure the connecting side");
        must(t.secure(d, s, Role::Server), "secure the accepting side");
    });
    let secured = |h: &Happened| matches!(h, Happened::Secured { result: 0, .. });
    until(rig, &mut seen, "the handshakes", |all| {
        count(all, secured) == 2
    });
    with(rig, |t, d| {
        t.bytes(d, from.buf())[..100].fill(0x3c);
        must(t.send(d, 0, from, false), "send");
    });
    let received = |h: &Happened| matches!(h, Happened::Received { slot, .. } if *slot == s);
    let mut opened = 0;
    for _ in 0..64 {
        // What the transport holds first; then it is asked for more.
        let room = Span::new(into.buf(), opened, into.len() - opened);
        let step = with(rig, |t, d| t.open(d, s, nothing, room));
        let Some(step) = step else {
            fail(format_args!("a record did not open: {seen:?}"));
        };
        opened += step.produced();
        if opened >= 100 {
            break;
        }
        let before = count(&seen, received);
        match with(rig, |t, d| t.recv_kept(d, s)) {
            Ok(true) => until(rig, &mut seen, "the kept receive", |all| {
                count(all, received) == before + 1
            }),
            Ok(false) => {}
            Err(Refused::Unsupported) if before == 0 => return,
            Err(e) => fail(format_args!("recv_kept: {e:?}")),
        }
    }
    let plain = with(rig, |t, d| t.bytes(d, into.buf())[..100].to_vec());
    if opened != 100 || plain != [0x3c; 100] {
        fail(format_args!("not the bytes sent: {opened} of them"));
    }
    // And one that a close ends.
    let before = count(&seen, received);
    with(rig, |t, d| {
        if must(t.recv_kept(d, s), "recv_kept") {
            must(t.close(d, s), "close");
        } else {
            fail(format_args!("nothing to receive into, with nothing kept"));
        }
    });
    let closed = |h: &Happened| matches!(h, Happened::Closed { slot } if *slot == s);
    until(
        rig,
        &mut seen,
        "the close and the receive it ended",
        |all| count(all, closed) == 1 && count(all, received) == before + 1,
    );
    let so_far = count(&seen, |h| of_slot(h, s));
    settle(rig, &mut seen);
    if count(&seen, |h| of_slot(h, s)) != so_far {
        fail(format_args!("reported again after the end: {seen:?}"));
    }
}
