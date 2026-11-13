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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

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

/// A waker that counts how often it was woken.
fn counted() -> (Arc<Count>, Waker) {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    (count.clone(), Waker::from(count))
}

struct Count(AtomicUsize);

impl Count {
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_waiter_polled_twice_leaves_the_next_buffer_to_the_other_waiter() {
    let mut r = rig(1);
    let (held0, held1) = (r.buffer(), r.buffer());
    let (b, wb) = counted();
    let (_a, wa) = counted();
    let (mut cb, mut ca) = (Context::from_waker(&wb), Context::from_waker(&wa));
    let client = r.client.clone();
    let mut first = pin!(client.buffer());
    let mut second = pin!(client.buffer());
    assert!(first.as_mut().poll(&mut cb).is_pending());
    // The second waiter is polled again while it waits, as under `select!`.
    assert!(second.as_mut().poll(&mut ca).is_pending());
    assert!(second.as_mut().poll(&mut ca).is_pending());
    drop(held0);
    let _taken = match second.as_mut().poll(&mut ca) {
        Poll::Ready(buffer) => buffer,
        Poll::Pending => panic!("a buffer was free"),
    };
    // The first waiter is polled whenever it is woken, as an executor would.
    if b.get() > 0 {
        assert!(first.as_mut().poll(&mut cb).is_pending());
    }
    let before = b.get();
    drop(held1);
    assert!(b.get() > before, "a buffer came back and its waiter slept on");
    assert!(first.as_mut().poll(&mut cb).is_ready());
}

#[test]
fn a_buffer_returned_on_another_lane_wakes_a_waiter() {
    let mut r = rig(2);
    // Two buffers per lane, taken from lane 0 first.
    let mut bufs: Vec<Buffer> = (0..4).map(|_| r.buffer()).collect();
    let (woken, waker) = counted();
    let mut cx = Context::from_waker(&waker);
    let client = r.client.clone();
    let mut waiting = pin!(client.buffer());
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    drop(bufs.pop());
    assert_eq!(woken.get(), 1, "a buffer came back on lane 1 and woke nobody");
    assert!(waiting.as_mut().poll(&mut cx).is_ready());
}

#[test]
fn a_blocking_borrow_takes_a_buffer_returned_on_another_lane() {
    let mut r = rig(2);
    let mut bufs: Vec<Buffer> = (0..4).map(|_| r.buffer()).collect();
    let client = r.client.clone();
    let (got, arrived) = mpsc::channel();
    let borrower = std::thread::spawn(move || {
        let buffer = client.buffer_blocking();
        got.send(()).unwrap();
        drop(buffer);
    });
    // Let the borrower find every buffer out and block.
    std::thread::sleep(Duration::from_millis(100));
    drop(bufs.pop());
    let took = arrived.recv_timeout(Duration::from_secs(5));
    // Unblock the borrower before failing, so the rig outlives it.
    drop(bufs);
    borrower.join().unwrap();
    assert!(took.is_ok(), "a buffer came back on lane 1 and the borrower slept on");
}

#[test]
fn an_operation_in_flight_when_the_io_fails_ends_closed_with_its_buffer() {
    let mut r = rig(1);
    let mut body = r.buffer();
    fill(&mut body, 3);
    let sent = body.as_slice()[..LEN].to_vec();
    let mut cx = Context::from_waker(Waker::noop());
    let mut storing = pin!(r.client.store("k", body, LEN));
    assert!(storing.as_mut().poll(&mut cx).is_pending());
    // The worker hands the store to the I/O, and the I/O then fails.
    let l = &mut r.lanes[0];
    l.worker.turn(&mut l.io);
    assert!(storing.as_mut().poll(&mut cx).is_pending());
    l.worker.abandon();
    let (result, body) = match storing.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("an abandoned operation is still pending"),
    };
    assert_eq!(result, Err(Error::Closed));
    assert_eq!(&body.as_slice()[..LEN], &sent[..]);
    // Nothing more is sent to the failed I/O.
    assert_eq!(r.run(r.client.erase("k")), Err(Error::Closed));
}
