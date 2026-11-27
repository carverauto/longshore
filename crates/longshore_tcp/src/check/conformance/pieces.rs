// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy receives (`Transport::recv_zc`): the bytes come as pieces of
//! pages, in order, never more than the credit; the receive ends with 0
//! when its credit is used, and with 0 when the stream ends first; a
//! close under one ends it, once, and nothing follows. Every piece is
//! returned once. A rig whose backend has no receive area ends the
//! receive with `-ENOBUFS` at once, and the statement is skipped there.

use super::{count, of_slot, pair, settle, until, with, Rig};
use crate::check::{fail, must};
use crate::transport::{Happened, Lend, Transport};
use longshore_common::io::{ECANCELED, ENOBUFS, ENOMEM, ENOSPC};

/// Bytes sent: most of two pages, all the rig's pool memory holds.
const SENT: u32 = 8000;

fn ended(slot: u32) -> impl Fn(&Happened) -> bool {
    move |h| matches!(h, Happened::RecvEnded { slot: s, .. } if *s == slot)
}

/// The bytes of every piece of `slot` reported after `from`, in order,
/// each returned once; and how the receive ended.
fn take_pieces<R: Rig>(rig: &mut R, seen: &[Happened], from: usize, slot: u32) -> (Vec<u8>, i32) {
    let mut got = Vec::new();
    let mut end = None;
    for h in &seen[from..] {
        if let Happened::RecvEnded { slot: s, result } = *h {
            if s == slot {
                end = Some(result);
            }
            continue;
        }
        let Happened::Piece {
            slot: s,
            page,
            off,
            len,
        } = *h
        else {
            continue;
        };
        if s != slot {
            continue;
        }
        if end.is_some() {
            fail(format_args!("a piece after the receive's end: {seen:?}"));
        }
        with(rig, |t, d| {
            let at = off as usize;
            got.extend_from_slice(&t.bytes(d, page)[at..at + len as usize]);
            t.give_back_piece(d, page, off, len);
        });
    }
    match end {
        Some(result) => (got, result),
        None => fail(format_args!("the receive did not end: {seen:?}")),
    }
}

/// Receive `credit` bytes on `slot` as `Net` does: armed for what is
/// left after an end for want of room (`-ENOSPC`), of a page for the
/// kernel's copy (`-ENOMEM`), or by a cancel; and cancelled, once, on a
/// turn the driver found no room to report in, where it may stay silent.
/// Its bytes, and how the last arming ended: 0, or `-ENOBUFS` at once.
fn receive_zc<R: Rig>(
    rig: &mut R,
    seen: &mut Vec<Happened>,
    slot: u32,
    credit: u32,
) -> (Vec<u8>, i32) {
    let mut got = Vec::new();
    for _ in 0..5_000 {
        let left = credit - got.len() as u32;
        let (from, before) = (seen.len(), count(seen, ended(slot)));
        with(rig, |t, d| must(t.recv_zc(d, slot, left), "recv_zc"));
        let (mut cancelled, mut full) = (false, with(rig, |t, d| t.ring_full_count(d)));
        for _ in 0..20_000 {
            if count(seen, ended(slot)) > before {
                break;
            }
            rig.turn(seen);
            let now = with(rig, |t, d| t.ring_full_count(d));
            if !cancelled && core::mem::replace(&mut full, now) < now {
                cancelled = with(rig, |t, d| t.cancel_recv(d, slot).is_ok());
            }
        }
        let (bytes, result) = take_pieces(rig, seen, from, slot);
        if bytes.len() > left as usize {
            fail(format_args!("{} bytes for a credit of {left}", bytes.len()));
        }
        got.extend_from_slice(&bytes);
        let short = got.len() < credit as usize;
        let again = result == -ENOSPC || result == -ENOMEM || result == -ECANCELED;
        match result {
            0 => return (got, 0),
            r if r == -ENOBUFS && got.is_empty() => return (got, r),
            _ if again && short => {}
            // A cancel that came after the last piece and before the end:
            // the credit is used all the same.
            r if r == -ECANCELED => return (got, 0),
            r => fail(format_args!(
                "a receive ended with {r}, {} of {credit} in",
                got.len()
            )),
        }
    }
    fail(format_args!(
        "a receive armed again and again brought only {}",
        got.len()
    ))
}

/// A receive with a credit of what was sent brings exactly that, in
/// order, and ends with 0; the next, with more credit than the peer
/// sends before it ends its stream, ends with 0 having brought less; and
/// one the slot is closed under ends once, then nothing more.
pub(super) fn a_zero_copy_receive_keeps_to_its_credit<R: Rig>(rig: &mut R) {
    let mut seen = Vec::new();
    let s = pair(rig, &mut seen);
    let from = rig.span(0, SENT);
    let byte = |i: usize| (i % 249) as u8 ^ 0x3c;
    with(rig, |t, d| {
        let all = &mut t.bytes(d, from.buf())[..SENT as usize];
        for (i, b) in all.iter_mut().enumerate() {
            *b = byte(i);
        }
        must(t.send(d, 0, from, Lend::No), "send");
    });
    let (first, result) = receive_zc(rig, &mut seen, s, 4096 + 100);
    if result == -ENOBUFS && first.is_empty() {
        // No receive area: the transport refuses the receive as it should.
        return;
    }
    let (rest, end) = receive_zc(rig, &mut seen, s, SENT - 4096 - 100);
    let got = [first, rest].concat();
    if result != 0 || end != 0 || got.len() != SENT as usize {
        fail(format_args!(
            "{} bytes for a credit of {SENT}, ends {result} and {end}",
            got.len()
        ));
    }
    if !got.iter().enumerate().all(|(i, b)| *b == byte(i)) {
        fail(format_args!("other bytes arrived than were sent"));
    }
    the_stream_ends_short_and_a_close_ends_a_receive(rig, &mut seen, s);
}

fn the_stream_ends_short_and_a_close_ends_a_receive<R: Rig>(
    rig: &mut R,
    seen: &mut Vec<Happened>,
    s: u32,
) {
    let from = rig.span(0, 100);
    with(rig, |t, d| must(t.send(d, 0, from, Lend::No), "send"));
    let sent = |h: &Happened| {
        matches!(
            h,
            Happened::Sent {
                slot: 0,
                result: 100
            }
        )
    };
    until(rig, seen, "the send", |all| count(all, sent) == 1);
    with(rig, |t, d| must(t.shutdown_write(d, 0), "shutdown"));
    let (got, end) = receive_zc(rig, seen, s, 5000);
    if end != 0 || got.len() != 100 {
        fail(format_args!(
            "{} bytes then {end} at the end of the stream",
            got.len()
        ));
    }
    // Armed, with nothing more to come: the close ends it.
    let at = (seen.len(), count(seen, ended(s)));
    with(rig, |t, d| {
        must(t.recv_zc(d, s, 4096), "recv_zc");
        must(t.close(d, s), "close");
    });
    let closed = |h: &Happened| matches!(h, Happened::Closed { slot } if *slot == s);
    until(rig, seen, "the close and the receive it ended", |all| {
        count(all, closed) == 1 && count(all, ended(s)) == at.1 + 1
    });
    let _end: (Vec<u8>, i32) = take_pieces(rig, seen, at.0, s);
    let so_far = count(seen, |h| of_slot(h, s));
    settle(rig, seen);
    if count(seen, |h| of_slot(h, s)) != so_far {
        fail(format_args!("reported again after the end: {seen:?}"));
    }
}
