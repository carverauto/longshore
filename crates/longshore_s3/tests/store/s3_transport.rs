// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! How the store reports responses the HTTP layer cannot use or cannot
//! finish: too large, not HTTP, cut short, never sent. Against the in-tree
//! HTTP server on one simulated `Io`, with heap allocation forbidden while
//! operations run.

#[cfg(test)]
mod common;
use common::*;
use longshore_http_client::client::Error;

const LEN: u32 = 6000;

/// A world holding one object at `k`, and the bytes of that object.
#[cfg(test)]
fn world_with_object(seed: u64, attempts_max: u32, enforce: bool) -> (World, Vec<u8>) {
    let mut w = world_with(seed, Faults::default(), attempts_max, enforce);
    w.fill_tx(LEN, seed as u8);
    let sent = w.io.buf(TX)[..LEN as usize].to_vec();
    w.s3.store(&mut w.io, 1, "k", TX, LEN, Checksum::Compute)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    (w, sent)
}

/// An object larger than the buffer it is fetched into is refused when its
/// length is known, and not asked for again: it would not fit then either.
#[test]
fn a_fetch_of_more_than_the_buffer_holds_is_refused_once() {
    let (mut w, _) = world_with_object(1, 4, false);
    w.server.script(Behavior::Raw(
        b"HTTP/1.1 200 OK\r\nContent-Length: 99999\r\n\r\n",
    ));
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::Transport(Error::BodyTooLarge)),
            attempts: 1
        }
    );
    // One byte more than fits is refused; exactly what fits is not.
    w.server.script(Behavior::Raw(Box::leak(
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BUF + 1)
            .into_bytes()
            .into_boxed_slice(),
    )));
    w.s3.fetch(&mut w.io, 3, "k", None, RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fatal(Fatal::Transport(Error::BodyTooLarge))
    );
    w.fill_tx(BUF, 3);
    w.s3.store(&mut w.io, 4, "full", TX, BUF, Checksum::None)
        .unwrap();
    assert_eq!(w.last().outcome, Outcome::Stored);
    w.s3.fetch(&mut w.io, 5, "full", None, RX).unwrap();
    assert_eq!(
        w.last().outcome,
        Outcome::Fetched {
            len: BUF,
            verified: false
        }
    );
}

/// A response that is not HTTP is reported as what it is, with the parse
/// error, and is not retried.
#[test]
fn a_response_that_is_not_http_is_fatal() {
    for (seed, behavior) in [(2, Behavior::GarbageHead), (3, Behavior::EndlessHead)] {
        let (mut w, _) = world_with_object(seed, 4, false);
        w.server.script(behavior);
        w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
        let d = w.last();
        assert!(
            matches!(d.outcome, Outcome::Fatal(Fatal::Transport(Error::Head(_)))),
            "{behavior:?}: {d:?}"
        );
        assert_eq!(d.attempts, 1, "{behavior:?}");
        assert_eq!(w.s3.in_progress(), 0);
    }
}

/// A body cut short is never handed over as the object: the fetch is made
/// again and returns every byte.
#[test]
fn a_body_cut_short_is_fetched_again_in_full() {
    for enforce in [false, true] {
        let (mut w, sent) = world_with_object(4, 4, enforce);
        w.server.script(Behavior::TruncateBody);
        w.io.buf(RX).fill(0);
        w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
        assert_eq!(
            w.last(),
            Done {
                token: 2,
                outcome: Outcome::Fetched {
                    len: LEN,
                    verified: enforce
                },
                attempts: 2
            },
            "checksums {enforce}"
        );
        assert!(w.io.buf(RX)[..LEN as usize] == sent[..]);
    }
    // Cut short every time: a failure, never a short object.
    let (mut w, _) = world_with_object(5, 2, false);
    w.server.script(Behavior::TruncateBody);
    w.server.script(Behavior::TruncateBody);
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: 2
        }
    );
}

/// A body sent in chunks is put together into the bytes of the object,
/// whatever the size of the chunks.
#[test]
fn a_chunked_body_arrives_as_the_object() {
    // Sizes whose framing still fits the test server's buffer.
    for chunk in [7, 1000, LEN, LEN + 1] {
        let (mut w, sent) = world_with_object(6, 3, false);
        w.server.script(Behavior::ChunkedBody(chunk));
        w.io.buf(RX).fill(0);
        w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
        assert_eq!(
            w.last(),
            Done {
                token: 2,
                outcome: Outcome::Fetched {
                    len: LEN,
                    verified: false
                },
                attempts: 1
            },
            "chunks of {chunk}"
        );
        assert!(
            w.io.buf(RX)[..LEN as usize] == sent[..],
            "chunks of {chunk}"
        );
    }
    // The test server sends no checksum with a chunked body. A store that
    // must send one is asked again rather than believed.
    let (mut w, sent) = world_with_object(6, 3, true);
    w.server.script(Behavior::ChunkedBody(1000));
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 2,
            outcome: Outcome::Fetched {
                len: LEN,
                verified: true
            },
            attempts: 2
        }
    );
    assert!(w.io.buf(RX)[..LEN as usize] == sent[..]);
}

/// A fetch the server never answers times out and is made again; when every
/// attempt goes unanswered the caller is told so.
#[test]
fn a_fetch_that_is_never_answered_times_out() {
    let (mut w, sent) = world_with_object(7, 2, false);
    w.server.script(Behavior::Stall);
    w.s3.fetch(&mut w.io, 2, "k", None, RX).unwrap();
    let d = w.last();
    assert_eq!(
        (d.outcome, d.attempts),
        (
            Outcome::Fetched {
                len: LEN,
                verified: false
            },
            2
        )
    );
    assert!(w.io.buf(RX)[..LEN as usize] == sent[..]);

    w.server.script(Behavior::Stall);
    w.server.script(Behavior::Stall);
    w.s3.fetch(&mut w.io, 3, "k", None, RX).unwrap();
    assert_eq!(
        w.last(),
        Done {
            token: 3,
            outcome: Outcome::Fatal(Fatal::RetriesExhausted),
            attempts: 2
        }
    );
}
