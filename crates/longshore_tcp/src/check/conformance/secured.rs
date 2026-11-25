// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! TLS handshakes, on a rig whose transport has TLS for both ends.

use super::{count, of_slot, pair, settle, until, with, Rig};
use crate::check::{fail, must};
use crate::transport::{Happened, Role, Transport};

fn secured(h: &Happened) -> bool {
    matches!(h, Happened::Secured { .. })
}

/// Both ends of a connection start a handshake: each is reported once,
/// as a success.
pub(super) fn a_handshake_is_reported_once<R: Rig>(rig: &mut R) {
    let Some(name) = rig.server_name() else {
        return;
    };
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    with(rig, |t, d| {
        if !(t.can_secure(true) && t.can_secure(false)) {
            fail(format_args!("a rig with a server name cannot secure"));
        }
        let client = Role::Client {
            server_name: name.as_str(),
        };
        must(t.secure(d, 0, client), "secure the connecting side");
        must(t.secure(d, s, Role::Server), "secure the accepting side");
    });
    let done = |slot: u32| move |h: &Happened| matches!(h, Happened::Secured { slot: at, result: 0 } if *at == slot);
    until(rig, &mut seen, "the handshakes", |all| {
        count(all, done(0)) == 1 && count(all, done(s)) == 1
    });
    settle(rig, &mut seen);
    if count(&seen, secured) != 2 {
        fail(format_args!("a handshake was reported again: {seen:?}"));
    }
}

/// A connection is closed under a handshake its peer never answers: the
/// handshake is reported once, as failed, the close once, and then
/// nothing more for the slot.
pub(super) fn a_close_under_a_handshake_ends_it<R: Rig>(rig: &mut R) {
    let Some(name) = rig.server_name() else {
        return;
    };
    let mut seen = Vec::new();
    let _: u32 = pair(rig, &mut seen);
    with(rig, |t, d| {
        let client = Role::Client {
            server_name: name.as_str(),
        };
        must(t.secure(d, 0, client), "secure");
    });
    settle(rig, &mut seen);
    if count(&seen, secured) != 0 {
        fail(format_args!("secured with no peer answering: {seen:?}"));
    }
    with(rig, |t, d| must(t.close(d, 0), "close"));
    let failed = |h: &Happened| matches!(h, Happened::Secured { slot: 0, result } if *result < 0);
    let closed = |h: &Happened| matches!(h, Happened::Closed { slot: 0 });
    until(rig, &mut seen, "the handshake's end and the close", |all| {
        count(all, failed) == 1 && count(all, closed) == 1
    });
    let so_far = count(&seen, |h| of_slot(h, 0));
    settle(rig, &mut seen);
    if count(&seen, |h| of_slot(h, 0)) != so_far || count(&seen, secured) != 1 {
        fail(format_args!("reported again after the end: {seen:?}"));
    }
}
