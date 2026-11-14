// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use longshore::client::{Builder, Client, Error};
use longshore::http::server::{self, Server};
use longshore::io::uring::{Fault, UringIo};
use longshore::io::{Completions, Io, Limits, Sock};
use longshore::store::creds::Credentials;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod churn;

pub const SIZE: u32 = 256 * 1024;
const CALLERS: usize = 4;
const CONNS: u32 = 4;

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |flag: &str, default: u32| {
        args.iter()
            .position(|a| a == flag)
            .map(|i| args[i + 1].parse().expect("a number"))
            .unwrap_or(default)
    };
    // A caller that never gets an answer is the failure these runs look
    // for, so a run that has not ended by now has failed.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(240));
        eprintln!("client_faults: still running after 240 s; a caller is stuck");
        std::process::exit(2);
    });
    match args.first().map(String::as_str) {
        Some("break") => {
            let fault = match args.get(1).map(String::as_str) {
                Some("tick") => Fault::TickFails,
                Some("ring") => Fault::RingLost,
                _ => panic!("break tick|ring"),
            };
            break_threads(fault, number("--threads", 2));
        }
        Some("churn") => churn::run(number("--rounds", 2000)),
        _ => panic!("client_faults break tick|ring [--threads N] | churn [--rounds N]"),
    }
}

/// Start the in-tree server on a thread of its own, with room for
/// `connections`, and return where it listens. It serves until the process
/// ends.
pub fn serve(connections: u32) -> SocketAddr {
    let (bound, listening) = channel();
    std::thread::spawn(move || {
        let mut io = UringIo::new(Limits {
            sockets_max: 1 + connections,
            buffers: 2 * connections,
            buffer_size: SIZE,
            small_buffers: 0,
            small_buffer_size: 0,
            ops_max: connections * 3 + 2,
        })
        .expect("server ring");
        let mut server = Server::new(server::Config {
            listener: Sock(0),
            connections_max: connections,
            first_sock: 1,
            first_buf: 0,
            head_max: 4096,
            buffer_size: SIZE,
            objects_max: 64,
            token_base: 2 << 40,
            checksum: None,
        });
        let addr = server
            .start(&mut io, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("server start");
        bound.send(addr).unwrap();
        let mut out = Completions::new(64);
        loop {
            io.tick(true, &mut out).expect("server tick");
            while let Some(c) = out.pop() {
                server.on_completion(&mut io, c);
            }
        }
    });
    listening.recv().expect("the server did not start")
}

pub fn builder(addr: SocketAddr, threads: u32) -> Builder {
    let mut b = Builder::new(
        "bucket.objects.test",
        "us-test-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    );
    b.port = addr.port();
    b.address = Some(addr);
    b.threads = threads;
    b.connections = CONNS;
    b.buffers = 2;
    b.operations = 4;
    b.buffer_size = SIZE;
    b
}

/// The client's I/O threads alive in this process, by their names.
pub fn io_threads() -> usize {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    tasks
        .flatten()
        .filter(|t| {
            std::fs::read_to_string(t.path().join("comm"))
                .is_ok_and(|name| name.starts_with("longshore-io-"))
        })
        .count()
}

pub fn descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

/// Wait up to five seconds for `what()` to become `want`.
pub fn settles(name: &str, want: usize, what: fn() -> usize) {
    let started = Instant::now();
    while what() != want {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{name}: {} and not {want}",
            what()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Default)]
struct Seen {
    stored: AtomicU64,
    closed: AtomicU64,
    void: AtomicU64,
}

/// One caller: store and fetch its own key until told to stop. An answer
/// of `Closed` is expected once threads fail. Anything else is a failure.
fn caller(client: Client, n: usize, stop: Arc<AtomicBool>, seen: Arc<Seen>) {
    let key = format!("caller-{n}");
    let len = SIZE as usize;
    let mut round = 0u8;
    while !stop.load(Ordering::Acquire) {
        round = round.wrapping_add(1);
        let mut body = client.buffer_blocking();
        if body.is_empty() {
            // Void: its thread failed and could not be stopped, or no
            // thread is left. An operation given it must end closed.
            seen.void.fetch_add(1, Ordering::Relaxed);
            let (stored, _void) = client.store(&key, body, 0).wait();
            assert_eq!(stored, Err(Error::Closed), "a void buffer was accepted");
            seen.closed.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        body.as_mut_slice().fill(round ^ n as u8);
        let (stored, body) = client.store(&key, body, len).wait();
        match stored {
            Ok(()) => {}
            Err(Error::Closed) => {
                seen.closed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            Err(other) => panic!("caller {n}: store ended {other:?}"),
        }
        let (fetched, body) = client.fetch(&key, body).wait();
        match fetched {
            Ok(Some(got)) => {
                assert_eq!(got, len, "caller {n}: fetched length");
                assert!(
                    body.as_slice().iter().all(|b| *b == round ^ n as u8),
                    "caller {n}: fetched bytes differ"
                );
                seen.stored.fetch_add(1, Ordering::Relaxed);
            }
            // The object may be on no thread's answer: the fetch went to a
            // thread that failed.
            Err(Error::Closed) => {
                seen.closed.fetch_add(1, Ordering::Relaxed);
            }
            other => panic!("caller {n}: fetch ended {other:?}"),
        }
    }
}

fn break_threads(fault: Fault, threads: u32) {
    assert!(
        threads >= 2,
        "break needs a thread to fail and one to go on"
    );
    let addr = serve(threads * CONNS);
    let (threads_before, fds_before) = (io_threads(), descriptors());
    let client = builder(addr, threads).build().expect("build");
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Seen::default());
    let callers: Vec<_> = (0..CALLERS)
        .map(|n| {
            let (c, s, k) = (client.clone(), stop.clone(), seen.clone());
            std::thread::spawn(move || caller(c, n, s, k))
        })
        .collect();
    let count = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let reaches = |what: &str, a: &AtomicU64, target: u64| {
        let started = Instant::now();
        while count(a) < target {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{what}: {} of {target} after 20 s",
                count(a)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // Every thread is carrying work before one fails.
    reaches("round trips before the fault", &seen.stored, 200);

    // Wait for the thread to notice. It may have had nothing in flight
    // just then, so no caller need have seen an operation end closed.
    let noticed = |thread: usize| {
        let started = Instant::now();
        while !client.thread_failed(thread) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "I/O thread {thread} never noticed its ring had failed"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    client.inject_fault(0, fault);
    noticed(0);
    let after_first = count(&seen.stored);
    // The threads still running carry on.
    reaches(
        "round trips after one thread failed",
        &seen.stored,
        after_first + 200,
    );
    let closed_by_first = count(&seen.closed);

    for thread in 1..threads as usize {
        client.inject_fault(thread, fault);
    }
    (1..threads as usize).for_each(noticed);
    // With no thread left every call still returns, and returns closed.
    reaches(
        "operations ended closed with every thread failed",
        &seen.closed,
        closed_by_first + 200,
    );
    let settled = count(&seen.stored);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        count(&seen.stored),
        settled,
        "an operation succeeded with every I/O thread failed"
    );
    if fault == Fault::RingLost {
        assert!(count(&seen.void) > 0, "no buffer came back void");
    }
    stop.store(true, Ordering::Release);
    for c in callers {
        c.join().expect("a caller failed");
    }
    // The failed threads are still there, refusing work, until the last
    // handle goes.
    assert_eq!(io_threads(), threads_before + threads as usize);
    drop(client);
    settles("I/O threads after the drop", threads_before, io_threads);
    settles("descriptors after the drop", fds_before, descriptors);
    println!(
        "ok break {fault:?}: {threads} I/O threads, {} round trips, {} operations ended closed, {} void buffers, threads and descriptors gone",
        count(&seen.stored),
        count(&seen.closed),
        count(&seen.void)
    );
}
