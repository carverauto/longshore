// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the model's tests are built from: an I/O thread running the real
//! worker over the simulator, woken through a signal the model checker
//! sees, and a small executor for the callers.

use crate::common::*;
use longshore::client::{Client, Layout, Worker};
use longshore::io::{Busy, FileId, Wake};
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::sync::{Condvar, Mutex};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

/// Run `f` under the model checker. Two forced thread switches reach the
/// orders that lose a wake-up, and keep a test to seconds.
pub fn model(f: impl Fn() + Sync + Send + 'static) {
    model_within(2, f);
}

/// As [`model`], with `switches` forced thread switches. Tests of four
/// threads use one, which keeps them to seconds as well.
pub fn model_within(switches: usize, f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(switches);
    let f = Arc::new(f);
    let orders = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = orders.clone();
    // The model's first thread has a small stack, so the test runs on one
    // of its own.
    builder.check(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let f = f.clone();
        spawn(move || f()).join().unwrap();
    });
    let orders = orders.load(std::sync::atomic::Ordering::Relaxed);
    println!("{orders} orders run");
    assert!(orders > 1, "the model checker found nothing to reorder");
}

/// Spawn a thread of the model with room for a store and a simulator.
pub fn spawn<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> loom::thread::JoinHandle<T> {
    loom::thread::Builder::new()
        .stack_size(1 << 21)
        .spawn(f)
        .unwrap()
}

/// How an I/O thread is woken, in place of the simulator's own flag, which
/// the model checker cannot see.
#[derive(Default)]
pub struct Signal {
    raised: Mutex<bool>,
    changed: Condvar,
}

impl Wake for Signal {
    fn wake(&self) {
        *self.raised.lock().unwrap() = true;
        self.changed.notify_one();
    }
}

impl Signal {
    fn wait(&self) {
        let mut raised = self.raised.lock().unwrap();
        while !*raised {
            raised = self.changed.wait(raised).unwrap();
        }
        *raised = false;
    }
}

/// The simulator, with [`Signal`] as its waker.
struct Woken {
    io: SimIo,
    signal: Arc<Signal>,
}

impl Io for Woken {
    fn now_ns(&self) -> u64 {
        self.io.now_ns()
    }
    fn unix_secs(&self) -> u64 {
        self.io.unix_secs()
    }
    fn buf(&mut self, id: BufId) -> &mut [u8] {
        self.io.buf(id)
    }
    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.io.buf_pair(a, b)
    }
    fn register_file(&mut self, path: &str) -> std::io::Result<FileId> {
        self.io.register_file(path)
    }
    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> std::io::Result<SocketAddr> {
        self.io.listen(sock, addr)
    }
    fn waker(&mut self) -> Option<Arc<dyn Wake>> {
        Some(self.signal.clone())
    }
    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        self.io.submit(token, op)
    }
    fn tick(&mut self, wait: bool, out: &mut Completions) -> std::io::Result<()> {
        self.io.tick(wait, out)
    }
}

/// What a lane's I/O thread does.
#[derive(Clone, Copy)]
pub enum Fate {
    /// Runs until the test ends.
    Serve,
    /// Its I/O fails after it has been woken `after_turns` times. `quiet`
    /// is whether the I/O could be stopped: see `Worker::abandon`.
    Fail { quiet: bool, after_turns: u32 },
}

/// One lane's I/O thread: the worker, its simulated machine and server.
pub struct Machine {
    io: Woken,
    worker: Worker,
    server: Server,
    out: Completions,
    fate: Fate,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
}

// A machine is built on the test's thread and then moved to its own.
unsafe impl Send for Machine {}

impl Machine {
    /// Take what callers asked for, and run the simulated I/O until the
    /// store has nothing in progress.
    fn turn(&mut self) {
        self.worker.turn(&mut self.io);
        while self.worker.store().in_progress() > 0 {
            self.io.tick(true, &mut self.out).expect("simulation");
            while let Some(c) = self.out.pop() {
                if !self.worker.on_completion(&mut self.io, c) {
                    assert!(self.server.on_completion(&mut self.io, c));
                }
            }
            self.worker.turn(&mut self.io);
        }
    }

    /// The I/O thread's loop, as `client::threads` runs it.
    fn run(mut self) -> Machine {
        let mut turns = 0;
        loop {
            if let Fate::Fail { quiet, after_turns } = self.fate {
                if turns == after_turns {
                    self.worker.abandon(quiet);
                    self.failed.store(true, Ordering::Release);
                    break;
                }
            }
            self.turn();
            if self.stop.load(Ordering::Acquire) {
                return self;
            }
            self.io.signal.wait();
            turns += 1;
        }
        // A failed thread ends whatever still arrives as closed.
        loop {
            self.worker.close();
            if self.stop.load(Ordering::Acquire) {
                return self;
            }
            self.io.signal.wait();
        }
    }
}

pub struct World {
    pub client: Client,
    threads: Vec<loom::thread::JoinHandle<Machine>>,
    signals: Vec<Arc<Signal>>,
    stop: Arc<AtomicBool>,
    failed: Vec<Arc<AtomicBool>>,
}

impl World {
    /// One lane per fate, each with `operations` slots and `buffers` lent
    /// buffers, and its I/O thread started.
    pub fn new(fates: &[Fate], operations: u32, buffers: u32) -> World {
        let stop = Arc::new(AtomicBool::new(false));
        let (mut lanes, mut machines, mut signals, mut failed) = (vec![], vec![], vec![], vec![]);
        for (n, &fate) in fates.iter().enumerate() {
            let signal = Arc::new(Signal::default());
            let limits = Limits {
                ops_max: LIMITS.ops_max + 2,
                ..LIMITS
            };
            let mut io = Woken {
                io: SimIo::new(limits, 40 + n as u64, Faults::default()),
                signal: signal.clone(),
            };
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
                buffers,
                token_base: 7 << 40,
                heartbeat_ns: 10 * MS,
            };
            let (mut worker, lane) = Worker::new(&mut io, S3::new(s3_config(3, false)), layout);
            worker.start(&mut io);
            lanes.push(lane);
            signals.push(signal);
            let flag = Arc::new(AtomicBool::new(false));
            failed.push(flag.clone());
            machines.push(Machine {
                io,
                worker,
                server,
                out: Completions::new(16),
                fate,
                stop: stop.clone(),
                failed: flag,
            });
        }
        // The machines are returned by their threads and dropped in `end`,
        // after the client.
        let client = unsafe { Client::over(lanes) };
        let threads = machines
            .into_iter()
            .map(|m| spawn(move || m.run()))
            .collect();
        World {
            client,
            threads,
            signals,
            stop,
            failed,
        }
    }

    /// Wait until lane `n`'s I/O has failed.
    pub fn failed(&self, n: usize) {
        while !self.failed[n].load(Ordering::Acquire) {
            loom::thread::yield_now();
        }
    }

    /// Stop the I/O threads, then drop the client before their machines.
    pub fn end(self) {
        self.stop.store(true, Ordering::Release);
        for s in &self.signals {
            s.wake();
        }
        let machines: Vec<Machine> = self
            .threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        drop(self.client);
        drop(machines);
    }
}

struct Unpark(loom::thread::Thread);

impl std::task::Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Run a future on this thread, parked between polls.
pub fn block_on<F: Future>(f: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(loom::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        loom::thread::park();
    }
}

/// Poll once with a waker that does nothing.
pub fn poll_once<F: Future>(f: Pin<&mut F>) -> Poll<F::Output> {
    f.poll(&mut Context::from_waker(Waker::noop()))
}
