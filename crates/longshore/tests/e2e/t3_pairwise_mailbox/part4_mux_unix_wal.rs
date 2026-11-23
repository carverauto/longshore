// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-28`–`T3-30`: `Stream` + mailbox multiplexing,
//! Unix-socket `Sender::stop()` shutdown-before-close, and `alloc_guard`-sealed WAL over mailbox.

use super::PingMsg;
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::{mailbox, queue, Config, TryRecv};

#[cfg(unix)]
#[test]
fn t3_28_stream_and_mailbox_receiver_multiplexed_on_one_io() {
    use super::super::harness::{drive_until, posix_backend, read_pool_bytes, write_pool_bytes};
    use longshore::stream::{Step, Stream};

    const WAKE_TOKEN: Token = Token(1 << 63);
    let limits = Limits {
        sockets_max: 6,
        buffers: 4,
        buffer_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).unwrap();
    let mut out = Completions::new(16);
    let (tx, mut io_rx) = queue::<PingMsg, _>(&mut io, 8, WAKE_TOKEN).unwrap();
    io_rx.arm(&mut io).unwrap();

    let addr = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut io, &mut out, 10_000, |v| v.len() == 2).unwrap();

    let mut cli = Stream::new(Sock(1), Token(10), Token(11));
    let mut srv = Stream::new(Sock(2), Token(20), Token(21));
    write_pool_bytes(&mut io, BufId(0), 0, b"mux-stream");
    cli.send(&mut io, BufId(0), 0, 10, u32::MAX);
    srv.recv(&mut io, BufId(1), 0, 16);
    tx.try_send(PingMsg { seq: 99, slot: 0 }).unwrap();

    let (mut got_msg, mut got_bytes) = (None, None);
    for _ in 0..10_000 {
        if got_msg.is_some() && got_bytes.is_some() {
            break;
        }
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if io_rx.on_completion(&mut io, c).unwrap() {
                while let TryRecv::Message(m) = io_rx.try_recv() {
                    got_msg = Some(m);
                }
            } else if c.token == Token(21) {
                if let Step::Done(n) = srv.on_recv(&mut io, c.result) {
                    got_bytes = Some(read_pool_bytes(&mut io, BufId(1), 0, n as usize));
                }
            } else if c.token == Token(10) {
                let _: Step = cli.on_send(&mut io, c.result);
            }
        }
    }
    io_rx.close(|_| {});
    assert_eq!(got_msg, Some(PingMsg { seq: 99, slot: 0 }));
    assert_eq!(got_bytes.as_deref(), Some(b"mux-stream".as_slice()));
}

#[cfg(unix)]
#[test]
fn t3_29_unix_socket_server_mailbox_stop_drains_and_closes() {
    use super::super::harness::{
        drive_until, posix_backend, submit_and_wait_one, write_nul_path, TempDir,
    };

    let dir = TempDir::new("t3_29").unwrap();
    let sock_p = dir.socket_path("stop.sock");
    let limits = Limits {
        sockets_max: 6,
        buffers: 2,
        buffer_size: 1024,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).unwrap();
    let mut out = Completions::new(16);
    io.listen_unix(Sock(0), sock_p.to_str().unwrap()).unwrap();
    let plen = write_nul_path(&mut io, BufId(0), 0, sock_p.to_str().unwrap());
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::ConnectUnix {
            sock: Sock(1),
            path_buf: BufId(0),
            path_off: 0,
            path_len: plen,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut io, &mut out, 10_000, |v| v.len() == 2).unwrap();

    let (tx, mut io_rx) = queue::<PingMsg, _>(&mut io, 4, Token(100)).unwrap();
    io_rx.arm(&mut io).unwrap();
    tx.try_send(PingMsg { seq: 1, slot: 0 }).unwrap();
    tx.try_send(PingMsg { seq: 2, slot: 0 }).unwrap();
    tx.stop();

    let mut drained = Vec::new();
    let mut stopped = false;
    for _ in 0..100 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if io_rx.on_completion(&mut io, c).unwrap() {
                loop {
                    match io_rx.try_recv() {
                        TryRecv::Message(m) => drained.push(m.seq),
                        TryRecv::Stop | TryRecv::Closed => {
                            stopped = true;
                            break;
                        }
                        TryRecv::Empty => break,
                    }
                }
            }
        }
        if stopped {
            break;
        }
    }
    io_rx.close(|_| {});
    assert!(stopped);
    assert_eq!(drained, vec![1, 2]);
    assert_eq!(
        submit_and_wait_one(
            &mut io,
            &mut out,
            Token(3),
            Op::Close { sock: Sock(2) },
            10_000
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut io,
            &mut out,
            Token(4),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 16
            },
            10_000
        )
        .unwrap()
        .result,
        0
    );
}

#[cfg(feature = "sim")]
#[test]
fn t3_30_sealed_wal_append_over_mailbox_within_prealloc_waiters() {
    use super::super::harness::{WalConfig, WalEngine};
    use longshore::checksum::Crc64Nvme;
    use longshore::io::alloc_guard::sealed;
    use longshore::io::FileSlot;
    use longshore::sim::{Faults, SimIo};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let limits = Limits {
        sockets_max: 2,
        buffers: 4,
        buffer_size: 4096,
        files_max: 2,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut sim = SimIo::new(limits, 330, Faults::default());
    sim.setup_disk(longshore::sim::DiskConfig {
        max_files: 8,
        max_dirs: 4,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: 32 * 1024,
        read_error_sectors: 4,
        sector_size: 512,
    });
    let mut out = Completions::new(8);
    let mut wal_cfg = WalConfig::new("", 4096);
    wal_cfg.write_buf = limits.aligned(0);
    wal_cfg.read_buf = limits.aligned(1);
    wal_cfg.direct = true;
    let mut wal = WalEngine::new(wal_cfg);
    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
        0
    );

    let (tx, mut io_rx) = mailbox::<PingMsg, (i32, i32), _>(
        &mut sim,
        Config {
            capacity: 8,
            slots: 4,
            waiters_prealloc: 8,
            wake_token: Token(900),
        },
    )
    .unwrap();
    io_rx.arm(&mut sim).unwrap();
    let payload = [0xD3u8; 64];
    let write_buf = limits.aligned(0);
    let waker = Waker::noop();

    let ok = sealed(|| {
        let mut cx = Context::from_waker(waker);
        let mut cursor = 0u64;
        for seq in 1..=4u64 {
            let mut fut = pin!(tx.call(|slot| PingMsg { seq, slot }, (-1, -1)));
            if !matches!(fut.as_mut().poll(&mut cx), Poll::Pending) {
                return false;
            }
            let mut pending_slot = None;
            let (mut w_res, mut s_res) = (-1i32, -1i32);
            let (mut got_w, mut got_s) = (false, false);
            for _ in 0..64 {
                if sim.tick(false, &mut out).is_err() {
                    return false;
                }
                while let Some(c) = out.pop() {
                    if io_rx.on_completion(&mut sim, c).unwrap_or(false) {
                        while let TryRecv::Message(m) = io_rx.try_recv() {
                            let mut crc = Crc64Nvme::new();
                            crc.update(&m.seq.to_le_bytes());
                            crc.update(&(payload.len() as u32).to_le_bytes());
                            crc.update(&payload);
                            let sum = crc.finish();
                            let b = sim.buf(write_buf);
                            b[..4096].fill(0);
                            b[0..4].copy_from_slice(&0x5741_4C31u32.to_le_bytes());
                            b[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
                            b[8..16].copy_from_slice(&m.seq.to_le_bytes());
                            b[16..24].copy_from_slice(&sum.to_le_bytes());
                            b[24..24 + payload.len()].copy_from_slice(&payload);
                            if sim
                                .submit_chain(&[
                                    (
                                        Token(1),
                                        Op::FileWrite {
                                            slot: FileSlot(0),
                                            buf: write_buf,
                                            off: 0,
                                            len: 4096,
                                            file_off: cursor,
                                        },
                                    ),
                                    (
                                        Token(2),
                                        Op::FileSync {
                                            slot: FileSlot(0),
                                            datasync: true,
                                        },
                                    ),
                                ])
                                .is_err()
                            {
                                return false;
                            }
                            pending_slot = Some(m.slot);
                        }
                    } else if c.token == Token(1) {
                        w_res = c.result;
                        got_w = true;
                    } else if c.token == Token(2) {
                        s_res = c.result;
                        got_s = true;
                    }
                }
                if got_w && got_s {
                    if let Some(s) = pending_slot.take() {
                        let _: Option<u32> = io_rx.answer(s, (w_res, s_res));
                    }
                    break;
                }
            }
            if !matches!(fut.as_mut().poll(&mut cx), Poll::Ready((4096, 0))) {
                return false;
            }
            cursor += 4096;
        }
        true
    });
    io_rx.close(|_| {}, || (-1, -1), |_| {});
    assert!(
        ok,
        "sealed WAL append over mailbox must succeed with 0 heap allocations"
    );
}
