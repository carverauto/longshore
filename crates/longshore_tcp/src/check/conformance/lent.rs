// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Sends whose memory the transport may read past their report
//! (`Lend::UntilReleased`): each is reported once, the next may follow
//! at once, and whatever was reported as still read is released once, by
//! its number, also when the slot is closed first.

use super::{count, of_slot, pair, receive, settle, until, with, Rig};
use crate::check::{fail, must};
use crate::transport::{Happened, Lend, Span, Transport};

const PIECE: u32 = 100;
const PIECES: u32 = 3;

fn reported(h: &Happened) -> bool {
    matches!(
        h,
        Happened::Sent { slot: 0, .. } | Happened::Taken { slot: 0, .. }
    )
}

/// Whether every number has been released as often as `due` says.
fn released(seen: &[Happened], due: &[usize; PIECES as usize]) -> bool {
    (0..PIECES).all(|id| {
        let of = |h: &Happened| matches!(h, Happened::Released { slot: 0, id: i } if *i == id);
        count(seen, of) == due[id as usize]
    })
}

/// Send piece `k` of `from` on slot 0 as lent send `id`, and turn until
/// it is reported. A send reported as still read is owed a release.
fn send_lent<R: Rig>(
    rig: &mut R,
    seen: &mut Vec<Happened>,
    from: Span,
    (k, id): (u32, u32),
    due: &mut [usize; PIECES as usize],
) {
    let before = count(seen, reported);
    let piece = Span::new(from.buf(), from.off() + k * PIECE, PIECE);
    with(rig, |t, d| {
        must(t.send(d, 0, piece, Lend::UntilReleased(id)), "a lent send");
    });
    until(rig, seen, "a lent send", |all| {
        count(all, reported) == before + 1
    });
    match seen.iter().rev().find(|h| reported(h)) {
        Some(Happened::Taken { result, .. }) if *result == PIECE.cast_signed() => {
            due[id as usize] += 1;
        }
        Some(Happened::Sent { result, .. }) if *result == PIECE.cast_signed() => {}
        other => fail(format_args!("a lent send of {PIECE} bytes: {other:?}")),
    }
}

/// Three sends, each started when the one before it was reported and not
/// when its memory came back; their bytes arrive in order; each is
/// released once if it was reported as still read. Then two more, and
/// the slot closed under them: released all the same, and nothing after.
pub(super) fn a_lent_send_is_reported_and_released_once<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let whole = (PIECES * PIECE) as usize;
    let (from, into) = (rig.span(0, PIECES * PIECE), rig.span(1, 4096));
    with(rig, |t, d| {
        let at = from.off() as usize;
        for (i, byte) in t.bytes(d, from.buf())[at..at + whole]
            .iter_mut()
            .enumerate()
        {
            *byte = (i % 251) as u8;
        }
    });
    let mut due = [0; PIECES as usize];
    for k in 0..PIECES {
        send_lent(rig, &mut seen, from, (k, k), &mut due);
    }
    let mut got = Vec::new();
    while got.len() < whole {
        let n = receive(rig, &mut seen, s, into);
        let Some(n) = usize::try_from(n).ok().filter(|n| *n > 0) else {
            fail(format_args!("the receive ended with {n}: {seen:?}"));
        };
        with(rig, |t, d| {
            let at = into.off() as usize;
            got.extend_from_slice(&t.bytes(d, into.buf())[at..at + n]);
        });
    }
    if !got.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8) {
        fail(format_args!("other bytes arrived than were sent"));
    }
    until(rig, &mut seen, "the release of every lent send", |all| {
        released(all, &due)
    });
    // Numbers that came back are used again; the slot is closed while
    // the transport may still read what these two sent.
    send_lent(rig, &mut seen, from, (0, 0), &mut due);
    send_lent(rig, &mut seen, from, (1, 1), &mut due);
    with(rig, |t, d| must(t.close(d, 0), "close"));
    let closed = |h: &Happened| matches!(h, Happened::Closed { slot: 0 });
    until(rig, &mut seen, "the close and the releases", |all| {
        count(all, closed) == 1 && released(all, &due)
    });
    let so_far = count(&seen, |h| of_slot(h, 0));
    settle(rig, &mut seen);
    if count(&seen, |h| of_slot(h, 0)) != so_far {
        fail(format_args!("reported again after the end: {seen:?}"));
    }
}
