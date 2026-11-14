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
    // --path-style addresses the bucket in the path. --rotate starts with
    // keys the server refuses and expects the client to pick up good ones
    // from its credentials file.
    let (mut path_style, mut rotate) = (false, false);
    let mut args = std::env::args().skip(1);
    while let Some(k) = args.next() {
        match k.as_str() {
            "--v6" => v6 = true,
            "--path-style" => path_style = true,
            "--rotate" => rotate = true,
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
        if path_style {
            server.serve_bucket_in_path("media");
        }
        if rotate {
            server.allow_access_keys(&["AKIDSECOND"]);
        }
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
    if path_style {
        b.host = "objects.test".into();
        b.bucket_in_path = Some("media".into());
    }
    let credentials_path = std::env::temp_dir().join(format!(
        "longshore-client-{}.credentials",
        std::process::id()
    ));
    if rotate {
        std::fs::write(
            &credentials_path,
            "[default]\naws_access_key_id = AKIDSECOND\naws_secret_access_key = second-secret\n",
        )
        .expect("write the credentials file");
        b.credentials_file = Some(credentials_path.clone());
        b.credentials_reread_secs = 1;
    }
    let client = b.build().unwrap_or_else(|e| {
        eprintln!("client_uring: {e}");
        std::process::exit(1);
    });
    assert_eq!(client.threads(), threads as usize);
    assert_eq!(thread_count(), threads_before + threads as usize);
    if rotate {
        // The keys the client was built with are refused. Every I/O thread
        // rereads the file within a second or two and switches; until all
        // have, a store may still land on a thread with the old keys.
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
                Err(other) => panic!("unexpected {other:?}"),
            }
        }
        assert!(refused > 0, "the first keys were supposed to be refused");
        let (adopted, _failed) = client.credential_reloads();
        assert!(
            adopted >= threads as u64,
            "every I/O thread adopts the new keys, {adopted} of {threads} did"
        );
        assert_eq!(client.erase("rotate-probe").wait(), Ok(()));
        let _ = std::fs::remove_file(&credentials_path);
        println!(
            "ok rotate: refused {refused} times with the first keys, then accepted with the file's"
        );
    }

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
    // A ring dropped with operations in flight first stops them all, well
    // inside quiesce's five-second bound: idle receives on connected
    // sockets, timers an hour away, a connect to an address nothing answers
    // and the wake-up read, over 200 at once, and every time.
    {
        use longshore::io::{Op, Token};
        const SOCKS: u32 = 100;
        const REPEATS: u32 = 50;
        let listener = std::net::TcpListener::bind((loopback, 0)).expect("listener");
        let peer = listener.local_addr().unwrap();
        let (accepted, round_accepted) = channel();
        let (finished, round_finished) = channel::<()>();
        // Holds each round's peers open, so the receives stay idle.
        let acceptor = std::thread::spawn(move || {
            for _ in 0..REPEATS {
                let peers: Vec<_> = (0..SOCKS).map(|_| listener.accept().unwrap()).collect();
                accepted.send(()).unwrap();
                round_finished.recv().unwrap();
                drop(peers);
            }
        });
        for round in 0..REPEATS {
            let limits = Limits {
                sockets_max: SOCKS + 1,
                buffers: 1,
                buffer_size: 8192,
                small_buffers: SOCKS,
                small_buffer_size: 64,
                ops_max: 2 * SOCKS + 2,
            };
            let mut io = UringIo::new(limits).expect("ring");
            let _wake = io.waker().expect("waker");
            let mut out = Completions::new(SOCKS as usize);
            for n in 0..SOCKS {
                let to = Op::Connect {
                    sock: Sock(n),
                    addr: peer,
                };
                io.submit(Token(n as u64), to).unwrap();
            }
            let mut connected = 0;
            while connected < SOCKS {
                io.tick(true, &mut out).expect("connect");
                while let Some(c) = out.pop() {
                    assert!(c.result >= 0, "connect failed: {}", c.result);
                    connected += 1;
                }
            }
            round_accepted.recv().unwrap();
            for n in 0..SOCKS {
                let (sock, buf) = (Sock(n), limits.small(n));
                let recv = Op::Recv {
                    sock,
                    buf,
                    off: 0,
                    len: 64,
                };
                io.submit(Token(1000 + n as u64), recv).unwrap();
                let hour = Op::Timeout {
                    nanos: 3_600 * 1_000_000_000,
                };
                io.submit(Token(2000 + n as u64), hour).unwrap();
            }
            // TEST-NET-1: routed nowhere, so the connect waits.
            let nowhere = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 9);
            io.submit(
                Token(9998),
                Op::Connect {
                    sock: Sock(SOCKS),
                    addr: nowhere,
                },
            )
            .unwrap();
            io.submit(Token(9999), Op::AwaitWake).unwrap();
            io.tick(false, &mut out).expect("submit");
            // A host with no route at all refuses the connect at once.
            while let Some(c) = out.pop() {
                assert_eq!(
                    c.token,
                    Token(9998),
                    "round {round}: an idle operation ended early"
                );
            }
            let started = std::time::Instant::now();
            assert!(
                io.quiesce(),
                "round {round}: operations in flight after quiesce"
            );
            let took = started.elapsed();
            assert!(
                took < std::time::Duration::from_secs(1),
                "round {round}: quiesce took {took:?}"
            );
            drop(io);
            finished.send(()).unwrap();
        }
        acceptor.join().expect("acceptor");
    }
    println!(
        "ok client: {threads} I/O threads, {CALLERS} caller threads, {} round trips of {SIZE} bytes, threads stopped on drop, over 200 operations quiesced 50 times",
        CALLERS * ROUNDS
    );

    /// The client's I/O threads alive in this process, by their names.
    /// Counting every task would also count the kernel's io_uring worker
    /// threads, which appear when a ring reads a file and linger for some
    /// seconds after their work is done.
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
}
