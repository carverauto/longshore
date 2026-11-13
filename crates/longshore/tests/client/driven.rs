// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The shareable client over workers driven by hand in the simulator: one
//! simulated machine per I/O lane, each with its own store and server.

#[path = "../store/common/mod.rs"]
mod common;
use common::*;
use longshore::client::{Client, Error, Layout, Output, Pending, Worker};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

const WIDE: Limits = Limits {
    ops_max: LIMITS.ops_max + 2,
    ..LIMITS
};
const LEN: usize = 1000;

/// One I/O lane: what would be one thread, with the server it talks to.
struct LaneRig {
    io: SimIo,
    worker: Worker,
    server: Server,
    out: Completions,
}

struct Rig {
    lanes: Vec<LaneRig>,
    client: Client,
}

fn rig(lanes: usize) -> Rig {
    let mut handles = Vec::new();
    let lanes: Vec<LaneRig> = (0..lanes)
        .map(|n| {
            let mut io = SimIo::new(WIDE, 40 + n as u64, Faults::default());
            let mut server = Server::new(server::Config {
                listener: Sock(0),
                connections_max: CONNS,
                first_sock: 1 + CONNS,
                first_buf: 0,
                head_max: 2048,
                buffer_size: BUF,
                objects_max: 8,
                token_base: 2 << 40,
                checksum: None,
            });
            server.start(&mut io, ADDR).unwrap();
            let layout = Layout {
                operations: 4,
                first_buffer: TX,
                buffers: 2,
                token_base: 7 << 40,
                heartbeat_ns: 10 * MS,
            };
            let (mut worker, lane) = Worker::new(&mut io, S3::new(s3_config(3, false)), layout);
            worker.start(&mut io);
            handles.push(lane);
            LaneRig {
                io,
                worker,
                server,
                out: Completions::new(16),
            }
        })
        .collect();
    Rig {
        lanes,
        // The lanes' simulators live in the rig beside the client.
        client: unsafe { Client::over(handles) },
    }
}

impl Rig {
    /// One turn of every lane's loop.
    fn pump(&mut self) {
        for l in &mut self.lanes {
            l.worker.turn(&mut l.io);
            l.io.tick(true, &mut l.out).expect("simulation deadlock");
            while let Some(c) = l.out.pop() {
                if !l.worker.on_completion(&mut l.io, c) {
                    assert!(l.server.on_completion(&mut l.io, c), "unrouted {c:?}");
                }
            }
            l.worker.turn(&mut l.io);
        }
    }

    /// Drive the lanes until `pending` ends.
    fn run<T: Output>(&mut self, pending: Pending<T>) -> T {
        let mut pending = pin!(pending);
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..100_000 {
            if let Poll::Ready(v) = pending.as_mut().poll(&mut cx) {
                return v;
            }
            self.pump();
        }
        panic!("the operation did not end");
    }

    fn buffer(&mut self) -> Buffer {
        let client = self.client.clone();
        let mut taking = pin!(client.buffer());
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..100_000 {
            if let Poll::Ready(b) = taking.as_mut().poll(&mut cx) {
                return b;
            }
            self.pump();
        }
        panic!("no buffer came back");
    }
}

use longshore::client::Buffer;

fn fill(buf: &mut Buffer, salt: u8) {
    for (i, b) in buf.as_mut_slice()[..LEN].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31) ^ salt;
    }
}

#[test]
fn a_body_goes_out_and_comes_back_through_lent_buffers() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 5);
    let sent = body.as_slice()[..LEN].to_vec();
    let (stored, body) = r.run(r.client.store("dir/a", body, LEN));
    assert_eq!(stored, Ok(()));
    assert_eq!(r.lanes[0].server.object_len("/dir/a"), Some(LEN as u32));

    let into = r.buffer();
    let (fetched, into) = r.run(r.client.fetch("dir/a", into));
    assert_eq!(fetched, Ok(Some(LEN)));
    assert_eq!(&into.as_slice()[..LEN], &sent[..]);

    let (ranged, into) = r.run(r.client.fetch_range("dir/a", 10, 19, into));
    assert_eq!(ranged, Ok(Some(10)));
    assert_eq!(&into.as_slice()[..10], &sent[10..20]);

    // The buffer `body` is still ours: the store handed it back.
    assert_eq!(&body.as_slice()[..LEN], &sent[..]);
}

#[test]
fn a_conditional_store_says_whether_it_stored() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 1);
    let (first, body) = r.run(r.client.store_no_clobber("k", body, LEN));
    assert_eq!(first, Ok(true));
    let (second, _body) = r.run(r.client.store_no_clobber("k", body, LEN));
    assert_eq!(second, Ok(false));
}

#[test]
fn a_missing_object_is_none_and_an_erase_of_nothing_succeeds() {
    let mut r = rig(1);
    let into = r.buffer();
    let (fetched, _into) = r.run(r.client.fetch("absent", into));
    assert_eq!(fetched, Ok(None));
    assert_eq!(r.run(r.client.erase("absent")), Ok(()));
}

#[test]
fn requests_that_cannot_be_sent_fail_at_once_and_return_the_buffer() {
    let mut r = rig(1);
    let body = r.buffer();
    let size = body.len();
    let (too_long, body) = r.run(r.client.store("k", body, size + 1));
    assert_eq!(too_long, Err(Error::TooLong));
    let (no_key, body) = r.run(r.client.store("", body, 1));
    assert_eq!(no_key, Err(Error::BadKey));
    let long_key = "k".repeat(longshore::client::KEY_MAX + 1);
    let (long, body) = r.run(r.client.fetch(&long_key, body));
    assert_eq!(long, Err(Error::BadKey));
    let (bad_range, _body) = r.run(r.client.fetch_range("k", 9, 3, body));
    assert_eq!(bad_range, Err(Error::BadRange));
    assert_eq!(r.run(r.client.erase("")), Err(Error::BadKey));
}

#[test]
fn more_operations_than_slots_wait_their_turn() {
    // Four operation slots, and twelve erases asked for at once.
    let mut r = rig(1);
    let mut cx = Context::from_waker(Waker::noop());
    let mut pending: Vec<_> = (0..12)
        .map(|i| Box::pin(r.client.erase(&format!("k{i}"))))
        .collect();
    let mut done = 0;
    for _ in 0..100_000 {
        pending.retain_mut(|p| match p.as_mut().poll(&mut cx) {
            Poll::Ready(result) => {
                assert_eq!(result, Ok(()));
                done += 1;
                false
            }
            Poll::Pending => true,
        });
        if pending.is_empty() {
            break;
        }
        r.pump();
    }
    assert_eq!(done, 12);
}

#[test]
fn a_dropped_operation_still_runs_and_its_buffer_comes_back_afterwards() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 9);
    let other = r.buffer();
    {
        let mut cx = Context::from_waker(Waker::noop());
        let mut storing = pin!(r.client.store("dropped", body, LEN));
        // Sent to the I/O side, then abandoned.
        assert!(storing.as_mut().poll(&mut cx).is_pending());
    }
    // Both buffers are out: one with us, one with the abandoned store.
    let mut cx = Context::from_waker(Waker::noop());
    {
        let client = r.client.clone();
        let mut taking = pin!(client.buffer());
        assert!(taking.as_mut().poll(&mut cx).is_pending());
    }
    // It finishes all the same, and only then does the buffer return.
    let back = r.buffer();
    assert_eq!(r.lanes[0].server.object_len("/dropped"), Some(LEN as u32));
    drop((back, other));
}

#[test]
fn work_spreads_over_the_lanes_and_a_buffer_stays_on_its_own() {
    let mut r = rig(2);
    assert_eq!(r.client.threads(), 2);
    // Four buffers in all, two per lane.
    let mut bufs: Vec<Buffer> = (0..4).map(|_| r.buffer()).collect();
    for (i, b) in bufs.iter_mut().enumerate() {
        fill(b, i as u8);
    }
    let mut cx = Context::from_waker(Waker::noop());
    let mut pending: Vec<_> = bufs
        .into_iter()
        .enumerate()
        .map(|(i, b)| Box::pin(r.client.store(&format!("obj{i}"), b, LEN)))
        .collect();
    for _ in 0..100_000 {
        pending.retain_mut(|p| match p.as_mut().poll(&mut cx) {
            Poll::Ready((result, _buffer)) => {
                assert_eq!(result, Ok(()));
                false
            }
            Poll::Pending => true,
        });
        if pending.is_empty() {
            break;
        }
        r.pump();
    }
    assert!(pending.is_empty());
    // Each lane's server received the two objects whose buffers were its own.
    let held = |l: &LaneRig| {
        (0..4)
            .filter(|i| l.server.object_len(&format!("/obj{i}")).is_some())
            .count()
    };
    assert_eq!((held(&r.lanes[0]), held(&r.lanes[1])), (2, 2));
}
