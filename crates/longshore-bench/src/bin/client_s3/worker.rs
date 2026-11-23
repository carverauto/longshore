// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The caller threads of `client_s3 put|get|mix`: each keeps its share of
//! the operations in flight through futures until told to stop.

use crate::{die, Args};
use longshore::client::{Buffer, Client, Error, Pending};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What the caller threads and the main thread share.
#[derive(Clone)]
pub struct Shared {
    pub bytes: Arc<AtomicU64>,
    pub requests: Arc<AtomicU64>,
    pub failed: Arc<AtomicU64>,
    /// For `mix`: stores begun, fetches ended, and fetches whose bytes were
    /// not the object's.
    pub puts: Arc<AtomicU64>,
    pub gets: Arc<AtomicU64>,
    pub wrong: Arc<AtomicU64>,
    pub measuring: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
    pub ready: Arc<Barrier>,
}

impl Shared {
    /// Counters at zero, and a barrier for `callers` threads and the main one.
    pub fn new(callers: u32) -> Self {
        Self {
            bytes: Arc::new(AtomicU64::new(0)),
            requests: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicU64::new(0)),
            puts: Arc::new(AtomicU64::new(0)),
            gets: Arc::new(AtomicU64::new(0)),
            wrong: Arc::new(AtomicU64::new(0)),
            measuring: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            ready: Arc::new(Barrier::new(callers as usize + 1)),
        }
    }
}

struct Unpark(std::thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// One operation in flight, of either kind.
enum Op {
    Put(Pending<(Result<(), Error>, Buffer)>),
    Get(Pending<(Result<Option<usize>, Error>, Buffer)>),
}

impl Op {
    fn begin(client: &Client, put: bool, key: &str, buffer: Buffer, size: usize) -> Op {
        if put {
            Op::Put(client.store(key, buffer, size))
        } else {
            Op::Get(client.fetch(key, buffer))
        }
    }

    /// Whether it ended well, and its buffer, once it has ended.
    fn poll(&mut self, cx: &mut Context<'_>, size: usize) -> Poll<(bool, Buffer)> {
        match self {
            Op::Put(p) => Pin::new(p).poll(cx).map(|(r, b)| (r == Ok(()), b)),
            Op::Get(p) => Pin::new(p).poll(cx).map(|(r, b)| (r == Ok(Some(size)), b)),
        }
    }

    fn is_put(&self) -> bool {
        matches!(self, Op::Put(_))
    }
}

/// The byte at `at` of the object a caller keeps under its key `i`.
fn pattern(c: u32, i: usize, at: usize) -> u8 {
    (at as u32).wrapping_mul(2654435761).to_le_bytes()[2] ^ (c as usize * 131 + i) as u8
}

/// Places in an object that a fetch checks: its ends and some between.
fn sampled(size: usize) -> impl Iterator<Item = usize> {
    (0..64).map(move |k| k * (size - 1) / 63)
}

/// Start the caller threads. They create their objects if they fetch, wait
/// at `shared.ready`, then run until `shared.stop`.
pub fn spawn(client: &Client, a: &Args, shared: &Shared) -> Vec<JoinHandle<()>> {
    // Each caller holds a share of the buffers, and the shares sum to all
    // of them, so no caller waits for one that another holds for good.
    let total = (a.threads * a.conns) as usize;
    let callers = a.callers as usize;
    let share = |c: usize| total / callers + usize::from(c < total % callers);
    // S3_TOLERATE_ERRORS=1 counts operations that fail and carries on, for
    // runs that break connections on purpose. Otherwise the first one ends
    // the run.
    let tolerate = std::env::var("S3_TOLERATE_ERRORS").is_ok_and(|v| v == "1");
    let started = Instant::now();
    let pid = std::process::id();
    (0..a.callers)
        .map(|c| {
            let caller = Caller {
                c,
                client: client.clone(),
                keys: (0..share(c as usize))
                    .map(|i| format!("bench/{pid}-c{c}-{i}.bin"))
                    .collect(),
                put: a.put,
                mix: a.mix,
                size: a.size,
                puts_per_sec: a.puts_per_sec,
                tolerate,
                started,
                shared: shared.clone(),
            };
            std::thread::spawn(move || {
                // For GET the objects must exist first.
                if !caller.put {
                    caller.create_objects();
                }
                let _: std::sync::BarrierWaitResult = caller.shared.ready.wait();
                caller.drive();
            })
        })
        .collect()
}

/// One caller thread and what it keeps in flight.
struct Caller {
    c: u32,
    client: Client,
    keys: Vec<String>,
    put: bool,
    mix: bool,
    size: u32,
    puts_per_sec: u64,
    tolerate: bool,
    started: Instant,
    shared: Shared,
}

impl Caller {
    fn fill(&self, i: usize, buffer: &mut Buffer) {
        for (at, byte) in buffer.as_mut_slice().iter_mut().enumerate() {
            *byte = pattern(self.c, i, at);
        }
    }

    /// In `mix`, the next operation on a key is a store while the run is
    /// behind its rate of stores, and a fetch if not.
    fn next(&self, i: usize, mut buffer: Buffer) -> Op {
        let (mix, puts) = (self.mix, &self.shared.puts);
        let due = self.started.elapsed().as_millis() as u64 * self.puts_per_sec / 1000;
        let store = self.put || (mix && puts.fetch_add(1, Ordering::Relaxed) < due);
        if mix && !store {
            let _: u64 = puts.fetch_sub(1, Ordering::Relaxed);
        }
        if mix && store {
            self.fill(i, &mut buffer);
        }
        Op::begin(
            &self.client,
            store,
            &self.keys[i],
            buffer,
            self.size as usize,
        )
    }

    /// Store every key once, so that fetches find it.
    fn create_objects(&self) {
        for (i, key) in self.keys.iter().enumerate() {
            let mut body = self.client.buffer_blocking();
            if self.mix {
                self.fill(i, &mut body);
            }
            let (stored, _b) = self.client.store(key, body, self.size as usize).wait();
            if stored.is_err() {
                die("could not create an object to fetch");
            }
        }
    }

    /// Keep an operation in flight on every key until told to stop.
    fn drive(&self) {
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let (c, size, mix, s) = (self.c, self.size, self.mix, &self.shared);
        let mut ops: Vec<(usize, Op)> = (0..self.keys.len())
            .map(|i| (i, self.next(i, self.client.buffer_blocking())))
            .collect();
        while !s.stop.load(Ordering::Relaxed) {
            let mut progressed = false;
            for (i, op) in ops.iter_mut() {
                if let Poll::Ready((ok, buffer)) = op.poll(&mut cx, size as usize) {
                    if !ok && !self.tolerate {
                        eprintln!("client_s3: an operation failed");
                        std::process::exit(3);
                    }
                    if !ok {
                        let _: u64 = s.failed.fetch_add(1, Ordering::Relaxed);
                    } else if s.measuring.load(Ordering::Relaxed) {
                        let _: u64 = s.bytes.fetch_add(u64::from(size), Ordering::Relaxed);
                        let _: u64 = s.requests.fetch_add(1, Ordering::Relaxed);
                    }
                    if mix && ok && !op.is_put() {
                        let _: u64 = s.gets.fetch_add(1, Ordering::Relaxed);
                        let got = buffer.as_slice();
                        if sampled(size as usize).any(|at| got[at] != pattern(c, *i, at)) {
                            let _: u64 = s.wrong.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    *op = self.next(*i, buffer);
                    progressed = true;
                }
            }
            if !progressed {
                std::thread::park_timeout(Duration::from_millis(50));
            }
        }
    }
}
