// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-28`–`T3-30`: `Stream` + mailbox multiplexing,
//! Unix-socket `Sender::stop()` shutdown-before-close, and `alloc_guard`-sealed WAL over mailbox.

use super::PingMsg;
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::{mailbox, queue, Config, QueueReceiver, Receiver, TryRecv};
#[cfg(feature = "sim")]
use longshore::sim::SimIo;

#[cfg(unix)]
#[test]
fn t3_28_stream_and_mailbox_receiver_multiplexed_on_one_io() {
    use super::super::harness::{drive_until, posix_backend, read_pool_bytes, write_pool_bytes};
    use longshore::stream::{Step, Stream};

    const WAKE_TOKEN: Token = Token::new(1 << 63);
    let limits = Limits::BASE
        .with_sockets_max(6)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_ops_max(8);
    let mut io = posix_backend(limits).unwrap();
    let mut out = Completions::new(16);
    let (tx, mut io_rx) = queue::<PingMsg, _>(&mut io, 8, WAKE_TOKEN).unwrap();
    io_rx.arm(&mut io).unwrap();

    let addr = io
        .listen(Sock::new(0), "127.0.0.1:0".parse().unwrap())
        .unwrap();
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(1),
            addr,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(&mut io, &mut out, 10_000, |v| v.len() == 2).unwrap();

    let mut cli = Stream::new(Sock::new(1), Token::new(10), Token::new(11));
    let mut srv = Stream::new(Sock::new(2), Token::new(20), Token::new(21));
    write_pool_bytes(&mut io, BufId::new(0), 0, b"mux-stream");
    cli.send(&mut io, BufId::new(0), 0, 10, u32::MAX);
    let _: Step = srv.recv(&mut io, BufId::new(1), 0, 16);
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
            } else if c.token().get() == 21 {
                if let Step::Done(n) = srv.on_recv(&mut io, c.result()) {
                    got_bytes = Some(read_pool_bytes(
                        &mut io,
                        BufId::new(1),
                        0,
                        usize::try_from(n).unwrap(),
                    ));
                }
            } else if c.token().get() == 10 {
                let _: Step = cli.on_send(&mut io, c.result());
            }
        }
    }
    io_rx.close(|_| {});
    assert_eq!(got_msg, Some(PingMsg { seq: 99, slot: 0 }));
    assert_eq!(got_bytes.as_deref(), Some(b"mux-stream".as_slice()));
}

/// Connects `Sock(1)` to `Sock(2)` over a Unix socket listening at `sock_p`.
#[cfg(unix)]
fn unix_pair<IO: Io>(io: &mut IO, out: &mut Completions, sock_p: &str) {
    use super::super::harness::{drive_until, write_nul_path};
    io.listen_unix(Sock::new(0), sock_p).unwrap();
    let plen = write_nul_path(io, BufId::new(0), 0, sock_p);
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(2),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::ConnectUnix {
            sock: Sock::new(1),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: plen,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(io, out, 10_000, |v| v.len() == 2).unwrap();
}

/// Ticks until the queue reports stop (or 100 ticks pass); returns the
/// drained sequence numbers and whether the stop was seen.
#[cfg(unix)]
fn drain_until_stop<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    io_rx: &mut QueueReceiver<PingMsg>,
) -> (Vec<u64>, bool) {
    let mut drained = Vec::new();
    let mut stopped = false;
    for _ in 0..100 {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if io_rx.on_completion(io, c).unwrap() {
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
    (drained, stopped)
}

#[cfg(unix)]
#[test]
fn t3_29_unix_socket_server_mailbox_stop_drains_and_closes() {
    use super::super::harness::{posix_backend, submit_and_wait_one, TempDir};

    let dir = TempDir::new("t3_29").unwrap();
    let sock_p = dir.socket_path("stop.sock");
    let limits = Limits::BASE
        .with_sockets_max(6)
        .with_buffers(2)
        .with_buffer_size(1024)
        .with_ops_max(8);
    let mut io = posix_backend(limits).unwrap();
    let mut out = Completions::new(16);
    unix_pair(&mut io, &mut out, sock_p.to_str().unwrap());

    let (tx, mut io_rx) = queue::<PingMsg, _>(&mut io, 4, Token::new(100)).unwrap();
    io_rx.arm(&mut io).unwrap();
    tx.try_send(PingMsg { seq: 1, slot: 0 }).unwrap();
    tx.try_send(PingMsg { seq: 2, slot: 0 }).unwrap();
    tx.stop();

    let (drained, stopped) = drain_until_stop(&mut io, &mut out, &mut io_rx);
    io_rx.close(|_| {});
    assert!(stopped);
    assert_eq!(drained, vec![1, 2]);
    assert_eq!(
        submit_and_wait_one(
            &mut io,
            &mut out,
            Token::new(3),
            Op::Close { sock: Sock::new(2) },
            10_000
        )
        .unwrap()
        .result(),
        0
    );
    assert_eq!(
        submit_and_wait_one(
            &mut io,
            &mut out,
            Token::new(4),
            Op::Recv {
                sock: Sock::new(1),
                buf: BufId::new(1),
                off: 0,
                len: 16
            },
            10_000
        )
        .unwrap()
        .result(),
        0
    );
}

/// Lays out one WAL record (magic, length, sequence, CRC-64/NVME, payload)
/// at the start of the zeroed block in `write_buf`.
#[cfg(feature = "sim")]
fn stage_wal_record(sim: &mut SimIo, write_buf: BufId, seq: u64, payload: &[u8]) {
    use longshore::checksum::Crc64Nvme;
    let mut crc = Crc64Nvme::new();
    crc.update(&seq.to_le_bytes());
    crc.update(&(payload.len() as u32).to_le_bytes());
    crc.update(payload);
    let sum = crc.finish();
    let b = sim.buf(write_buf);
    b[..4096].fill(0);
    b[0..4].copy_from_slice(&0x5741_4C31u32.to_le_bytes());
    b[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    b[8..16].copy_from_slice(&seq.to_le_bytes());
    b[16..24].copy_from_slice(&sum.to_le_bytes());
    b[24..24 + payload.len()].copy_from_slice(payload);
}

/// Chains the block write at `cursor` (`Token(1)`) with a datasync (`Token(2)`).
#[cfg(feature = "sim")]
fn submit_append_chain(
    sim: &mut SimIo,
    write_buf: BufId,
    cursor: u64,
) -> Result<(), longshore::io::Busy> {
    use longshore::io::FileSlot;
    sim.submit_chain(&[
        (
            Token::new(1),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: write_buf,
                off: 0,
                len: 4096,
                file_off: cursor,
            },
        ),
        (
            Token::new(2),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ])
}

/// Serves one queued append: writes and syncs its record at `cursor`, then
/// answers the caller with both results. Returns `false` on any I/O failure.
#[cfg(feature = "sim")]
fn serve_append(
    sim: &mut SimIo,
    out: &mut Completions,
    io_rx: &mut Receiver<PingMsg, (i32, i32)>,
    write_buf: BufId,
    payload: &[u8],
    cursor: u64,
) -> bool {
    let mut pending_slot = None;
    let (mut w_res, mut s_res) = (-1i32, -1i32);
    let (mut got_w, mut got_s) = (false, false);
    for _ in 0..64 {
        if sim.tick(false, out).is_err() {
            return false;
        }
        while let Some(c) = out.pop() {
            if io_rx.on_completion(sim, c).unwrap_or(false) {
                while let TryRecv::Message(m) = io_rx.try_recv() {
                    stage_wal_record(sim, write_buf, m.seq, payload);
                    if submit_append_chain(sim, write_buf, cursor).is_err() {
                        return false;
                    }
                    pending_slot = Some(m.slot);
                }
            } else if c.token().get() == 1 {
                w_res = c.result();
                got_w = true;
            } else if c.token().get() == 2 {
                s_res = c.result();
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
    true
}

/// A simulator with a disk and an open direct-I/O WAL segment in `FileSlot(0)`.
#[cfg(feature = "sim")]
fn sim_with_open_wal_segment(limits: Limits) -> (SimIo, Completions) {
    use super::super::harness::{WalConfig, WalEngine};
    use longshore::sim::Faults;

    let mut sim = SimIo::new(limits, 330, Faults::default());
    sim.setup_disk(
        longshore::sim::DiskConfig::BASE
            .with_max_files(8)
            .with_max_dirs(4)
            .with_max_bytes_per_file(64 * 1024)
            .with_max_name_len(128)
            .with_write_log_entries(16)
            .with_write_log_bytes(32 * 1024)
            .with_read_error_sectors(4)
            .with_sector_size(512),
    );
    let mut out = Completions::new(8);
    let wal_cfg = WalConfig::new("", 4096).with_io_bufs(limits.aligned(0), limits.aligned(1));
    let mut wal = WalEngine::new(wal_cfg);
    assert_eq!(
        wal.open_segment(&mut sim, &mut out, 1, true, true).unwrap(),
        0
    );
    (sim, out)
}

#[cfg(feature = "sim")]
#[test]
fn t3_30_sealed_wal_append_over_mailbox_within_prealloc_waiters() {
    use longshore::io::alloc_guard::sealed;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_files_max(2)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_ops_max(8);
    let (mut sim, mut out) = sim_with_open_wal_segment(limits);
    let (tx, mut io_rx) =
        mailbox::<PingMsg, (i32, i32), _>(&mut sim, Config::new(8, 4, 8, Token::new(900))).unwrap();
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
            if !serve_append(&mut sim, &mut out, &mut io_rx, write_buf, &payload, cursor) {
                return false;
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
