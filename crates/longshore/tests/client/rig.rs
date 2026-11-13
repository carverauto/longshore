// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Lanes driven by hand in the simulator, one simulated machine each with
//! its own store and server, and a client over them.

#![allow(dead_code)]

use crate::common::*;
use longshore::client::{Buffer, Client, Layout, Output, Pending, Worker};
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

const WIDE: Limits = Limits {
    ops_max: LIMITS.ops_max + 2,
    ..LIMITS
};
pub const LEN: usize = 1000;

/// One I/O lane: what would be one thread, with the server it talks to.
pub struct LaneRig {
    pub io: SimIo,
    pub worker: Worker,
    pub server: Server,
    pub out: Completions,
}

pub struct Rig {
    pub lanes: Vec<LaneRig>,
    pub client: Client,
}

pub fn rig(lanes: usize) -> Rig {
    rig_with(lanes, 4)
}

/// `lanes` lanes of `operations` operation slots and two buffers each.
pub fn rig_with(lanes: usize, operations: u32) -> Rig {
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
                operations,
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
    pub fn pump(&mut self) {
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
    pub fn run<T: Output>(&mut self, pending: Pending<T>) -> T {
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

    pub fn buffer(&mut self) -> Buffer {
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

pub fn fill(buf: &mut Buffer, salt: u8) {
    for (i, b) in buf.as_mut_slice()[..LEN].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31) ^ salt;
    }
}

/// A waker that counts how often it was woken.
pub fn counted() -> (Arc<Count>, Waker) {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    (count.clone(), Waker::from(count))
}

pub struct Count(AtomicUsize);

impl Count {
    pub fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
