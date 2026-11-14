// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Build a client and drop it with work in flight, many times over.

use super::{builder, descriptors, io_threads, serve, settles, SIZE};
use longshore::client::{Client, Error};
use longshore::io::uring::Fault;
use std::time::Duration;

const THREADS: u32 = 2;
const CALLERS: usize = 3;

/// Resident memory of this process, in KiB.
fn resident_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

/// What a caller does with its handle before letting it go.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Way {
    /// Finish a store and a fetch.
    Finish,
    /// Start operations and drop them unfinished.
    Detach,
    /// Finish a store while the main thread's handle is already gone.
    Outlive,
    /// As `Finish`, with a thread's ring failing underneath.
    Fail,
}

fn caller(client: Client, n: usize, way: Way) {
    let key = format!("churn-{n}");
    let mut body = client.buffer_blocking();
    let void = body.is_empty();
    assert!(!void || way == Way::Fail, "{way:?}: lent a void buffer");
    body.as_mut_slice().fill(n as u8);
    match way {
        Way::Detach => {
            // Neither is waited for: each takes its buffer with it.
            drop(client.store(&key, body, SIZE as usize));
            drop(client.erase(&key));
        }
        Way::Finish | Way::Outlive | Way::Fail => {
            let (stored, body) = client.store(&key, body, SIZE as usize).wait();
            match stored {
                Ok(()) if !void => {}
                Err(Error::Closed) if way == Way::Fail => return,
                other => panic!("{way:?}: store ended {other:?}"),
            }
            let (fetched, body) = client.fetch(&key, body).wait();
            match fetched {
                Ok(Some(got)) => {
                    assert_eq!(got, SIZE as usize);
                    assert!(body.as_slice().iter().all(|b| *b == n as u8));
                }
                Err(Error::Closed) if way == Way::Fail => {}
                other => panic!("{way:?}: fetch ended {other:?}"),
            }
        }
    }
}

/// `bounded_memory` is off under a sanitizer, which keeps freed memory.
pub fn run(rounds: u32, bounded_memory: bool) {
    // Each round's connections are closed when its client goes, and the
    // server learns of that a little later, so it has room for several
    // rounds at once.
    let addr = serve(THREADS * super::CONNS * 8);
    let (threads_before, fds_before) = (io_threads(), descriptors());
    let mut settled_kib = 0;
    for round in 0..rounds {
        let way = [Way::Finish, Way::Detach, Way::Outlive, Way::Fail][round as usize % 4];
        let client = builder(addr, THREADS).build().expect("build");
        let callers: Vec<_> = (0..CALLERS)
            .map(|n| {
                let c = client.clone();
                std::thread::spawn(move || caller(c, n, way))
            })
            .collect();
        match way {
            Way::Fail => client.inject_fault(round as usize % THREADS as usize, Fault::TickFails),
            Way::Outlive => {}
            Way::Finish | Way::Detach => {
                // Let some of the work get under way first.
                std::thread::sleep(Duration::from_micros(200 * (round as u64 % 5)));
            }
        }
        drop(client);
        for c in callers {
            c.join().expect("a caller failed");
        }
        // The last handle went on whichever thread dropped it last, and
        // that drop joined the I/O threads.
        settles("I/O threads after a round", threads_before, io_threads);
        if round == rounds / 4 {
            settled_kib = resident_kib();
        }
    }
    settles("descriptors after the rounds", fds_before, descriptors);
    let grown = resident_kib().saturating_sub(settled_kib);
    assert!(
        !bounded_memory || grown < 32 * 1024,
        "resident memory grew by {grown} KiB over the last three quarters of the rounds"
    );
    println!(
        "ok churn: {rounds} clients of {THREADS} I/O threads built and dropped with work in flight, threads and descriptors gone, memory grew {grown} KiB"
    );
}
