// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-26`–`T3-27`: portable `Client` waiter
//! saturation + `Drop` stop, and armed `Accept`/`Recv` frontend + mailbox backend.

use super::PingMsg;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::{mailbox, Config, Receiver, Sender, TryRecv};

#[test]
fn t3_26_s3_client_portable_waiter_saturation_and_drop_stop() {
    use super::super::harness::HttpStoreServer;
    use longshore::client::Builder;
    use longshore::store::creds::Credentials;

    let server = HttpStoreServer::start_plain().expect("server");
    let creds = Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .unwrap();
    let b = Builder::new("127.0.0.1", "us-east-1", creds)
        .port(server.port())
        .address(Some(server.addr()))
        .bucket_in_path(Some("sat".into()))
        .tls(false)
        .portable(true)
        .threads(1)
        .operations(2)
        .waiters(2)
        .buffers(4)
        .buffer_size(64 * 1024)
        .connections(4);
    let client = b.build().expect("client");

    let handles: Vec<_> = (0..6u32)
        .map(|i| {
            let c = client.clone();
            std::thread::spawn(move || {
                let mut buf = c.buffer_blocking();
                let key = format!("sat-{i}");
                buf.as_mut_slice()[..4].copy_from_slice(&i.to_le_bytes());
                let (st, buf) = c.store(&key, buf, 4).wait();
                st.expect("store under waiter saturation");
                drop(buf);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    drop(client);
    assert!(server.request_count() >= 6);
}

/// Runs a backend core on its own thread that answers each ping with
/// `seq ^ 0xA5A5` until stopped; returns its sender and the thread.
#[cfg(unix)]
fn spawn_ping_backend(be_limits: Limits) -> (Sender<PingMsg, u64>, std::thread::JoinHandle<()>) {
    use super::super::harness::posix_backend;
    let (tx_ready, rx_ready) = std::sync::mpsc::sync_channel(1);
    let be_handle = std::thread::spawn(move || {
        let mut be = posix_backend(be_limits).unwrap();
        let (tx_be, mut io_rx_be) = mailbox::<PingMsg, u64, _>(
            &mut be,
            Config {
                capacity: 8,
                slots: 4,
                waiters_prealloc: 4,
                wake_token: Token::new(1),
            },
        )
        .unwrap();
        io_rx_be.arm(&mut be).unwrap();
        tx_ready.send(tx_be).unwrap();

        let mut out = Completions::new(8);
        let mut stop = false;
        while !stop {
            be.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if io_rx_be.on_completion(&mut be, c).unwrap() {
                    stop = answer_pings(&mut io_rx_be);
                }
            }
        }
        io_rx_be.close(|_| {}, || 0, |_| {});
    });
    (rx_ready.recv().unwrap(), be_handle)
}

/// Answers every queued ping; returns `true` once the mailbox stops or closes.
#[cfg(unix)]
fn answer_pings(io_rx_be: &mut Receiver<PingMsg, u64>) -> bool {
    loop {
        match io_rx_be.try_recv() {
            TryRecv::Message(msg) => {
                assert!(io_rx_be.answer(msg.slot, msg.seq ^ 0xA5A5).is_none());
            }
            TryRecv::Stop | TryRecv::Closed => return true,
            TryRecv::Empty => return false,
        }
    }
}

/// Sends the eight bytes of `val` on `sock` from `buf`.
#[cfg(unix)]
fn send_u64<IO: Io>(io: &mut IO, token: Token, sock: Sock, buf: BufId, val: u64) {
    use super::super::harness::write_pool_bytes;
    write_pool_bytes(io, buf, 0, &val.to_le_bytes());
    io.submit(
        token,
        Op::Send {
            sock,
            buf,
            off: 0,
            len: 8,
            zero_copy: false,
        },
    )
    .unwrap();
}

/// Accepts a loopback connection on an armed listener; returns the server socket.
#[cfg(unix)]
fn armed_accept_pair<IO: Io>(fe: &mut IO, out_fe: &mut Completions) -> Sock {
    use super::super::harness::drive_until;
    let addr = fe
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    fe.submit(
        Token::new(10),
        Op::ArmedAccept {
            listener: Sock::new(0),
        },
    )
    .unwrap();
    fe.submit(
        Token::new(11),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(fe, out_fe, 10_000, |v| v.len() == 2).unwrap();
    Sock::new(u32::try_from(cs.iter().find(|c| c.token().get() == 10).unwrap().result()).unwrap())
}

#[cfg(unix)]
#[test]
fn t3_27_frontend_armed_recv_mailbox_backend_give_back() {
    use super::super::harness::{drive_until, posix_backend, read_pool_bytes, submit_and_wait_one};
    let fe_limits = Limits::BASE
        .with_sockets_max(12)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_accept_range_start(6)
        .with_accept_range_len(4)
        .with_ring_buffers(4)
        .with_ring_buffer_size(1024)
        .with_ops_max(16);
    let be_limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(64)
        .with_ops_max(4);
    let mut fe = posix_backend(fe_limits).unwrap();
    let (tx_be, be_handle) = spawn_ping_backend(be_limits);

    let mut out_fe = Completions::new(16);
    let srv = armed_accept_pair(&mut fe, &mut out_fe);
    fe.submit(Token::new(12), Op::ArmedRecv { sock: srv })
        .unwrap();

    send_u64(&mut fe, Token::new(13), Sock::new(1), BufId::new(0), 42);
    let rcs = drive_until(&mut fe, &mut out_fe, 10_000, |v| {
        v.iter().any(|c| c.token().get() == 12)
    })
    .unwrap();
    let rc = rcs.iter().find(|c| c.token().get() == 12).unwrap();
    let rbuf = rc.buf().unwrap();
    let raw = read_pool_bytes(&mut fe, rbuf, 0, 8);
    let seq = u64::from_le_bytes(raw.try_into().unwrap());
    fe.give_back(rbuf);

    let ans = tx_be.call(|slot| PingMsg { seq, slot }, 0).wait();
    tx_be.stop();
    be_handle.join().unwrap();
    assert_eq!(ans, 42 ^ 0xA5A5);

    send_u64(&mut fe, Token::new(14), srv, BufId::new(1), ans);
    let ack = submit_and_wait_one(
        &mut fe,
        &mut out_fe,
        Token::new(15),
        Op::Recv {
            sock: Sock::new(1),
            buf: BufId::new(2),
            off: 0,
            len: 8,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(ack.result(), 8);
    assert_eq!(
        u64::from_le_bytes(
            read_pool_bytes(&mut fe, BufId::new(2), 0, 8)
                .try_into()
                .unwrap()
        ),
        42 ^ 0xA5A5
    );
}
