// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The shareable client on real io_uring threads, against the in-tree server
//! on a thread of its own over loopback.
//!
//!   client_uring [--threads N] [--v6]
//!
//! Stores, fetches and erases objects from several caller threads at once,
//! through blocking calls and through futures, verifies every byte, uploads
//! an object in parts, writes one that is read back under the identity the
//! store gave it, ends a request at its deadline, then
//! drops the client and checks that its I/O threads have gone.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("client_uring needs Linux");
}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;
#[cfg(target_os = "linux")]
use die::OrDie;

#[cfg(target_os = "linux")]
#[path = "client_uring/exact.rs"]
mod exact;

#[cfg(target_os = "linux")]
#[path = "client_uring/multipart.rs"]
mod multipart;

#[cfg(target_os = "linux")]
#[path = "client_uring/server.rs"]
mod server;

#[cfg(target_os = "linux")]
#[path = "client_uring/callers.rs"]
mod callers;

#[cfg(target_os = "linux")]
#[path = "client_uring/quiesce.rs"]
mod quiesce;

/// The size of every object and of the buffers that carry them.
#[cfg(target_os = "linux")]
const SIZE: u32 = 1 << 20;
/// Caller threads storing and fetching at once.
#[cfg(target_os = "linux")]
const CALLERS: usize = 4;
/// Round trips each caller makes.
#[cfg(target_os = "linux")]
const ROUNDS: usize = 8;
/// Connections each I/O thread keeps to the server.
#[cfg(target_os = "linux")]
const CONNS_PER_THREAD: u32 = 4;

/// What the command line asked for.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Args {
    threads: u32,
    v6: bool,
    /// Addresses the bucket in the path.
    path_style: bool,
    /// Starts with keys the server refuses and expects the client to pick up
    /// good ones from its credentials file.
    rotate: bool,
}

#[cfg(target_os = "linux")]
impl Args {
    fn parse() -> Self {
        let mut a = Self {
            threads: 2,
            v6: false,
            path_style: false,
            rotate: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(k) = args.next() {
            match k.as_str() {
                "--v6" => a.v6 = true,
                "--path-style" => a.path_style = true,
                "--rotate" => a.rotate = true,
                "--threads" => {
                    a.threads = args
                        .next()
                        .and_then(|v| v.parse().ok())
                        .or_die("--threads N")
                }
                _ => die::die(format_args!("unknown flag {k}")),
            }
        }
        a
    }

    fn loopback(self) -> std::net::IpAddr {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        if self.v6 {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let args = Args::parse();
    let threads = args.threads;
    let addr = server::start(args);

    let threads_before = thread_count();
    let credentials_path = std::env::temp_dir().join(format!(
        "longshore-client-{}.credentials",
        std::process::id()
    ));
    let client = build(args, addr, &credentials_path);
    assert_eq!(client.threads(), threads as usize);
    assert_eq!(thread_count(), threads_before + threads as usize);
    if args.rotate {
        rotate(&client, threads, &credentials_path);
    }

    callers::run(&client);

    multipart::round_trip(&client, SIZE as usize / 4);
    exact::round_trip(&client, SIZE as usize / 4);
    exact::deadline_stops_a_connect();

    // A buffer still out keeps the I/O threads alive after the last handle.
    let kept = client.buffer_blocking();
    drop(client);
    // The client the deadline check built ends its thread on its own, after
    // cancelling the connect still waiting on an address nothing answers.
    assert_eq!(
        settles_at(threads_before + threads as usize),
        threads_before + threads as usize
    );
    drop(kept);
    assert_eq!(
        settles_at(threads_before),
        threads_before,
        "the I/O threads did not stop"
    );
    quiesce::check(args.loopback());
    println!(
        "ok client: {threads} I/O threads, {CALLERS} caller threads, {} round trips of {SIZE} bytes, a multipart upload of three parts, a write read back under its identity, threads stopped on drop, over 200 operations quiesced 50 times",
        CALLERS * ROUNDS
    );
}

/// The client against the server at `addr`, reading keys from
/// `credentials_path` when the keys are to rotate.
#[cfg(target_os = "linux")]
fn build(
    args: Args,
    addr: std::net::SocketAddr,
    credentials_path: &std::path::Path,
) -> longshore::client::Client {
    use longshore::client::Builder;
    use longshore::store::creds::Credentials;
    let mut b = Builder::new(
        "bucket.objects.test",
        "us-test-1",
        Credentials::new("AKIDEXAMPLE", "secret").or_die("Credentials::new"),
    );
    b.port = addr.port();
    b.address = Some(addr);
    b.threads = args.threads;
    b.connections = CONNS_PER_THREAD;
    b.buffers = 2;
    b.operations = 4;
    b.buffer_size = SIZE;
    if args.path_style {
        b.host = "objects.test".into();
        b.bucket_in_path = Some("media".into());
    }
    if args.rotate {
        std::fs::write(
            credentials_path,
            "[default]\naws_access_key_id = AKIDSECOND\naws_secret_access_key = second-secret\n",
        )
        .or_die("write the credentials file");
        b.credentials_file = Some(credentials_path.to_path_buf());
        b.credentials_reread_secs = 1;
    }
    b.build().unwrap_or_else(|e| {
        eprintln!("client_uring: {e}");
        std::process::exit(1);
    })
}

/// The keys the client was built with are refused. Every I/O thread
/// rereads the file within a second or two and switches; until all
/// have, a store may still land on a thread with the old keys.
#[cfg(target_os = "linux")]
fn rotate(client: &longshore::client::Client, threads: u32, credentials_path: &std::path::Path) {
    use longshore::client::Error;
    let started = std::time::Instant::now();
    let mut refused = 0;
    let mut accepted_in_a_row = 0;
    while accepted_in_a_row < 4 * threads {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the client never adopted the credentials in its file"
        );
        let (stored, _body) = client
            .store("rotate-probe", client.buffer_blocking(), 16)
            .wait();
        match stored {
            Ok(()) => accepted_in_a_row += 1,
            Err(Error::Store(_)) => {
                refused += 1;
                accepted_in_a_row = 0;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(other) => die::die(format_args!("unexpected {other:?}")),
        }
    }
    assert!(refused > 0, "the first keys were supposed to be refused");
    let (adopted, _failed) = client.credential_reloads();
    assert!(
        adopted >= u64::from(threads),
        "every I/O thread adopts the new keys, {adopted} of {threads} did"
    );
    assert_eq!(client.erase("rotate-probe").wait(), Ok(()));
    // Best-effort cleanup: a leftover temporary file does not change the result.
    let _: Option<()> = std::fs::remove_file(credentials_path).ok();
    println!(
        "ok rotate: refused {refused} times with the first keys, then accepted with the file's"
    );
}

/// The number of I/O threads once it reaches `want`, or after six seconds.
/// Threads end on their own once the last handle and buffer are gone,
/// after stopping what they still have in flight, which quiesce bounds at
/// five seconds; counting at once would race that.
#[cfg(target_os = "linux")]
fn settles_at(want: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        let n = thread_count();
        if n == want || std::time::Instant::now() >= deadline {
            return n;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The client's I/O threads alive in this process, by their names.
/// Counting every task would also count the kernel's io_uring worker
/// threads, which appear when a ring reads a file and linger for some
/// seconds after their work is done.
#[cfg(target_os = "linux")]
fn thread_count() -> usize {
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
