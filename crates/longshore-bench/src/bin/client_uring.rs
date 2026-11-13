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
//! through blocking calls and through futures, verifies every byte, then
//! drops the client and checks that its I/O threads have gone.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("client_uring needs Linux");
}

#[cfg(target_os = "linux")]
fn main() {
    use longshore::client::{Builder, Client, Error};
    use longshore::http::server::{self, Server};
    use longshore::io::uring::UringIo;
    use longshore::io::{Completions, Io, Limits, Sock};
    use longshore::store::creds::Credentials;
    use std::future::Future;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::mpsc::channel;
    use std::task::{Context, Poll, Wake, Waker};

    const SIZE: u32 = 1 << 20;
    const CALLERS: usize = 4;
    const ROUNDS: usize = 8;

    let (mut threads, mut v6) = (2u32, false);
    let mut args = std::env::args().skip(1);
    while let Some(k) = args.next() {
        match k.as_str() {
            "--v6" => v6 = true,
            "--threads" => {
                threads = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--threads N")
            }
            _ => panic!("unknown flag {k}"),
        }
    }
    let conns_per_thread = 4;
    let loopback = if v6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };

    // The server: its own ring on its own thread, serving until the process ends.
    let (bound, listening) = channel();
    let server_conns = threads * conns_per_thread;
    std::thread::spawn(move || {
        let mut io = UringIo::new(Limits {
            sockets_max: 1 + server_conns,
            buffers: 2 * server_conns,
            buffer_size: SIZE,
            small_buffers: 0,
            small_buffer_size: 0,
            ops_max: server_conns * 3 + 2,
        })
        .expect("server ring");
        let mut server = Server::new(server::Config {
            listener: Sock(0),
            connections_max: server_conns,
            first_sock: 1,
            first_buf: 0,
            head_max: 4096,
            buffer_size: SIZE,
            objects_max: (CALLERS * 2) as u32,
            token_base: 2 << 40,
            checksum: None,
        });
        let addr = server
            .start(&mut io, SocketAddr::new(loopback, 0))
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
    let addr = listening.recv().expect("the server did not start");

    let threads_before = thread_count();
    let mut b = Builder::new(
        "bucket.objects.test",
        "us-test-1",
        Credentials::new("AKIDEXAMPLE", "secret").unwrap(),
    );
    b.port = addr.port();
    b.address = Some(addr);
    b.threads = threads;
    b.connections = conns_per_thread;
    b.buffers = 2;
    b.operations = 4;
    b.buffer_size = SIZE;
    let client = b.build().unwrap_or_else(|e| {
        eprintln!("client_uring: {e}");
        std::process::exit(1);
    });
    assert_eq!(client.threads(), threads as usize);
    assert_eq!(thread_count(), threads_before + threads as usize);

    /// A minimal executor: polls on this thread, parked between wake-ups.
    fn block_on<F: Future>(f: F) -> F::Output {
        struct Unpark(std::thread::Thread);
        impl Wake for Unpark {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(std::sync::Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park();
        }
    }

    fn pattern(caller: usize, round: usize, i: usize) -> u8 {
        (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ (caller * 31 + round) as u8
    }

    // Each caller thread stores, fetches, checks and erases its own keys.
    // Even callers block; odd callers await futures.
    let callers: Vec<_> = (0..CALLERS)
        .map(|caller| {
            let client: Client = client.clone();
            std::thread::spawn(move || {
                let key = format!("caller-{caller}");
                let len = SIZE as usize;
                for round in 0..ROUNDS {
                    let asynchronous = caller % 2 == 1;
                    let mut body = if asynchronous {
                        block_on(client.buffer())
                    } else {
                        client.buffer_blocking()
                    };
                    for (i, b) in body.as_mut_slice().iter_mut().enumerate() {
                        *b = pattern(caller, round, i);
                    }
                    let (stored, body) = if asynchronous {
                        block_on(client.store(&key, body, len))
                    } else {
                        client.store(&key, body, len).wait()
                    };
                    assert_eq!(stored, Ok(()), "caller {caller} round {round}");
                    drop(body);
                    let into = client.buffer_blocking();
                    let (fetched, into) = if asynchronous {
                        block_on(client.fetch(&key, into))
                    } else {
                        client.fetch(&key, into).wait()
                    };
                    assert_eq!(fetched, Ok(Some(len)), "caller {caller} round {round}");
                    let ok = into
                        .as_slice()
                        .iter()
                        .enumerate()
                        .all(|(i, b)| *b == pattern(caller, round, i));
                    assert!(ok, "caller {caller} round {round}: bytes differ");
                    let (ranged, into) = client.fetch_range(&key, 100, 199, into).wait();
                    assert_eq!(ranged, Ok(Some(100)));
                    assert!(into.as_slice()[..100]
                        .iter()
                        .enumerate()
                        .all(|(i, b)| *b == pattern(caller, round, 100 + i)));
                    drop(into);
                }
                assert_eq!(client.erase(&key).wait(), Ok(()));
                let into = client.buffer_blocking();
                let (gone, into) = client.fetch(&key, into).wait();
                assert_eq!(gone, Ok(None), "caller {caller}: erased object still there");
                // Give it back first: the pool may hold only one buffer per caller.
                drop(into);
                let (no_key, _b): (Result<(), Error>, _) =
                    client.store("", client.buffer_blocking(), 1).wait();
                assert_eq!(no_key, Err(Error::BadKey));
            })
        })
        .collect();
    for c in callers {
        c.join().expect("a caller thread failed");
    }

    // A buffer still out keeps the I/O threads alive after the last handle.
    let kept = client.buffer_blocking();
    drop(client);
    assert_eq!(thread_count(), threads_before + threads as usize);
    drop(kept);
    assert_eq!(
        thread_count(),
        threads_before,
        "the I/O threads did not stop"
    );
    // A ring dropped with operations in flight first stops them: a wake-up
    // read and a timer far in the future both end, and promptly.
    {
        use longshore::io::{Op, Token};
        let mut io = UringIo::new(Limits {
            sockets_max: 1,
            buffers: 1,
            buffer_size: 8192,
            small_buffers: 0,
            small_buffer_size: 0,
            ops_max: 2,
        })
        .expect("ring");
        let _wake = io.waker().expect("waker");
        io.submit(Token(1), Op::AwaitWake).unwrap();
        io.submit(Token(2), Op::Timeout { nanos: 3_600 * 1_000_000_000 }).unwrap();
        io.tick(false, &mut Completions::new(4)).expect("submit");
        let started = std::time::Instant::now();
        assert!(io.quiesce(), "operations still in flight after quiesce");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        drop(io);
    }
    println!(
        "ok client: {threads} I/O threads, {CALLERS} caller threads, {} round trips of {SIZE} bytes, threads stopped on drop, ring quiesced",
        CALLERS * ROUNDS
    );

    fn thread_count() -> usize {
        std::fs::read_dir("/proc/self/task")
            .map(|d| d.count())
            .unwrap_or(0)
    }
}
