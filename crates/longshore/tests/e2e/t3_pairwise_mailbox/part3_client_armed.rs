// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-26`–`T3-27`: portable `Client` waiter
//! saturation + `Drop` stop, and armed `Accept`/`Recv` frontend + mailbox backend.

use super::PingMsg;
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::{mailbox, Config, TryRecv};

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
    let mut b = Builder::new("127.0.0.1", "us-east-1", creds);
    b.port = server.port();
    b.address = Some(server.addr());
    b.bucket_in_path = Some("sat".into());
    b.tls = false;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.threads = 1;
    b.operations = 2;
    b.waiters = 2;
    b.buffers = 4;
    b.buffer_size = 64 * 1024;
    b.connections = 4;
    let client = b.build().expect("client");

    let handles: Vec<_> = (0..6)
        .map(|i| {
            let c = client.clone();
            std::thread::spawn(move || {
                let mut buf = c.buffer_blocking();
                let key = format!("sat-{i}");
                buf.as_mut_slice()[..4].copy_from_slice(&(i as u32).to_le_bytes());
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

#[cfg(unix)]
#[test]
fn t3_27_frontend_armed_recv_mailbox_backend_give_back() {
    use super::super::harness::{
        drive_until, posix_backend, read_pool_bytes, submit_and_wait_one, write_pool_bytes,
    };
    let fe_limits = Limits {
        sockets_max: 12,
        buffers: 4,
        buffer_size: 4096,
        accept_range_start: 6,
        accept_range_len: 4,
        ring_buffers: 4,
        ring_buffer_size: 1024,
        ops_max: 16,
        ..Limits::BASE
    };
    let be_limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 64,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut fe = posix_backend(fe_limits).unwrap();
    let (tx_ready, rx_ready) = std::sync::mpsc::sync_channel(1);

    let be_handle = std::thread::spawn(move || {
        let mut be = posix_backend(be_limits).unwrap();
        let (tx_be, mut io_rx_be) = mailbox::<PingMsg, u64, _>(
            &mut be,
            Config {
                capacity: 8,
                slots: 4,
                waiters_prealloc: 4,
                wake_token: Token(1),
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
                    loop {
                        match io_rx_be.try_recv() {
                            TryRecv::Message(msg) => {
                                assert!(io_rx_be.answer(msg.slot, msg.seq ^ 0xA5A5).is_none());
                            }
                            TryRecv::Stop | TryRecv::Closed => {
                                stop = true;
                                break;
                            }
                            TryRecv::Empty => break,
                        }
                    }
                }
            }
        }
        io_rx_be.close(|_| {}, || 0, |_| {});
    });
    let tx_be = rx_ready.recv().unwrap();

    let mut out_fe = Completions::new(16);
    let addr = fe.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
    fe.submit(Token(10), Op::ArmedAccept { listener: Sock(0) })
        .unwrap();
    fe.submit(
        Token(11),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let cs = drive_until(&mut fe, &mut out_fe, 10_000, |v| v.len() == 2).unwrap();
    let srv = Sock(cs.iter().find(|c| c.token == Token(10)).unwrap().result as u32);
    fe.submit(Token(12), Op::ArmedRecv { sock: srv }).unwrap();

    write_pool_bytes(&mut fe, BufId(0), 0, &42u64.to_le_bytes());
    fe.submit(
        Token(13),
        Op::Send {
            sock: Sock(1),
            buf: BufId(0),
            off: 0,
            len: 8,
            zero_copy: false,
        },
    )
    .unwrap();
    let rcs = drive_until(&mut fe, &mut out_fe, 10_000, |v| {
        v.iter().any(|c| c.token == Token(12))
    })
    .unwrap();
    let rc = rcs.iter().find(|c| c.token == Token(12)).unwrap();
    let rbuf = rc.buf.unwrap();
    let raw = read_pool_bytes(&mut fe, rbuf, 0, 8);
    let seq = u64::from_le_bytes(raw.try_into().unwrap());
    fe.give_back(rbuf);

    let ans = tx_be.call(|slot| PingMsg { seq, slot }, 0).wait();
    tx_be.stop();
    be_handle.join().unwrap();
    assert_eq!(ans, 42 ^ 0xA5A5);

    write_pool_bytes(&mut fe, BufId(1), 0, &ans.to_le_bytes());
    fe.submit(
        Token(14),
        Op::Send {
            sock: srv,
            buf: BufId(1),
            off: 0,
            len: 8,
            zero_copy: false,
        },
    )
    .unwrap();
    let ack = submit_and_wait_one(
        &mut fe,
        &mut out_fe,
        Token(15),
        Op::Recv {
            sock: Sock(1),
            buf: BufId(2),
            off: 0,
            len: 8,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(ack.result, 8);
    assert_eq!(
        u64::from_le_bytes(read_pool_bytes(&mut fe, BufId(2), 0, 8).try_into().unwrap()),
        42 ^ 0xA5A5
    );
}
