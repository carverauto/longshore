// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-02`: Multi-core partitioned storage service (`2x Engine<Virtual>` +
//! `longshore::mailbox` + `submit_chain` + Direct I/O + network/disk faults).

use super::super::harness::{write_nul_path, write_pool_bytes};
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token};
use longshore::mailbox::{mailbox, Config, Receiver, TryRecv};
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

const BLOCK: u32 = 4096;
const LISTENER: Sock = Sock(0);
const CLIENT_SOCK: Sock = Sock(1);
const ACCEPT_START: u32 = 2;
const ACCEPT_LEN: u32 = 4;

#[derive(Clone, Copy, Debug)]
enum DiskReq {
    Put { key: u32, val: u64, slot: u32 },
    Get { key: u32, slot: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiskResp {
    PutOk(i32),
    GetOk(i32, u64),
}

fn net_limits() -> Limits {
    Limits {
        sockets_max: 8,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 256,
        accept_range_start: ACCEPT_START,
        accept_range_len: ACCEPT_LEN,
        ring_buffers: 8,
        ring_buffer_size: 512,
        ops_max: 16,
        ..Limits::BASE
    }
}

fn disk_limits() -> Limits {
    Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 256,
        aligned_buffers: 4,
        aligned_buffer_size: BLOCK,
        block_size: BLOCK,
        files_max: 4,
        ops_max: 16,
        ..Limits::BASE
    }
}

fn setup_disk_sim(seed: u64) -> SimIo {
    let mut sim = SimIo::new(disk_limits(), seed, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 8,
        max_dirs: 8,
        max_bytes_per_file: (BLOCK as usize) * 16,
        max_name_len: 64,
        write_log_entries: 16,
        write_log_bytes: (BLOCK as usize) * 8,
        read_error_sectors: 8,
        sector_size: 512,
    });
    sim.set_disk_faults(DiskFaults {
        short_write: 50_000,
        bit_flip: 20_000,
        ..DiskFaults::default()
    });
    sim
}

fn drain_disk(
    disk: &mut SimIo,
    lim: &Limits,
    out: &mut Completions,
    io_rx: &mut Receiver<DiskReq, DiskResp>,
) {
    while let TryRecv::Message(req) = io_rx.try_recv() {
        handle_disk_req(disk, lim, out, io_rx, req);
    }
}

fn handle_disk_req(
    disk: &mut SimIo,
    lim: &Limits,
    out: &mut Completions,
    io_rx: &mut Receiver<DiskReq, DiskResp>,
    req: DiskReq,
) {
    let (path_buf, data_buf) = (lim.small(0), lim.aligned(0));
    let path_len = write_nul_path(disk, path_buf, 0, "/kv_part.dat");
    let open = |write: bool| Op::FileOpen {
        slot: FileSlot(0),
        path_buf,
        path_off: 0,
        path_len,
        flags: OpenFlags {
            read: true,
            write,
            create: write,
            direct: true,
            ..Default::default()
        },
    };
    let close = Op::FileClose { slot: FileSlot(0) };
    let pump_until = |d: &mut SimIo, o: &mut Completions, end_tok: u64| -> (i32, i32) {
        let (mut r1, mut r2, mut done) = (0i32, 0i32, false);
        while !done {
            d.tick(true, o).unwrap();
            while let Some(c) = o.pop() {
                if c.token.0 % 100 == 1 {
                    r1 = c.result;
                } else if c.token.0 % 100 == 2 && c.token.0 != end_tok {
                    r2 = c.result;
                }
                if c.token.0 == end_tok {
                    done = true;
                }
            }
        }
        (r1, r2)
    };
    match req {
        DiskReq::Put { key, val, slot } => {
            let mut block = [0u8; BLOCK as usize];
            block[0..4].copy_from_slice(&key.to_le_bytes());
            block[4..12].copy_from_slice(&val.to_le_bytes());
            write_pool_bytes(disk, data_buf, 0, &block);
            let wr = Op::FileWrite {
                slot: FileSlot(0),
                buf: data_buf,
                off: 0,
                len: BLOCK,
                file_off: u64::from(key) * u64::from(BLOCK),
            };
            let sy = Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            };
            disk.submit_chain(&[
                (Token(100), open(true)),
                (Token(101), wr),
                (Token(102), sy),
                (Token(103), close),
            ])
            .unwrap();
            let (write_res, sync_res) = pump_until(disk, out, 103);
            let status = if write_res == BLOCK as i32 && sync_res == 0 {
                0
            } else if sync_res < 0 {
                sync_res
            } else {
                write_res
            };
            let _: Option<u32> = io_rx.answer(slot, DiskResp::PutOk(status));
        }
        DiskReq::Get { key, slot } => {
            let rd = Op::FileRead {
                slot: FileSlot(0),
                buf: data_buf,
                off: 0,
                len: BLOCK,
                file_off: u64::from(key) * u64::from(BLOCK),
            };
            disk.submit_chain(&[
                (Token(200), open(false)),
                (Token(201), rd),
                (Token(202), close),
            ])
            .unwrap();
            let (read_res, _) = pump_until(disk, out, 202);
            let val = if read_res >= 12 {
                u64::from_le_bytes(disk.buf(data_buf)[4..12].try_into().unwrap())
            } else {
                0
            };
            let _: Option<u32> = io_rx.answer(slot, DiskResp::GetOk(read_res, val));
        }
    }
}

fn run_partitioned_service(net_seed: u64, disk_seed: u64) -> (u64, u64, Vec<(u32, DiskResp)>) {
    let net_faults = Faults {
        delay: 150_000,
        partial_recv: 200_000,
        short_send: 200_000,
        ..Faults::default()
    };
    let mut net = SimIo::new(net_limits(), net_seed, net_faults);
    let mut disk = setup_disk_sim(disk_seed);
    let dlim = disk_limits();

    let (tx, mut io_rx) = mailbox::<DiskReq, DiskResp, _>(
        &mut disk,
        Config {
            capacity: 8,
            slots: 8,
            waiters_prealloc: 8,
            wake_token: Token(900),
        },
    )
    .expect("mailbox");
    let mut net_out = Completions::new(16);
    let mut disk_out = Completions::new(16);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let addr = net
        .listen(LISTENER, "127.0.0.1:9100".parse().unwrap())
        .unwrap();
    net.submit(Token(1), Op::ArmedAccept { listener: LISTENER })
        .unwrap();
    net.submit(
        Token(2),
        Op::Connect {
            sock: CLIENT_SOCK,
            addr,
        },
    )
    .unwrap();

    let mut accepted: Option<Sock> = None;
    let mut connected = false;
    for _ in 0..100 {
        net.tick(false, &mut net_out).unwrap();
        while let Some(c) = net_out.pop() {
            if c.token.0 == 1 && c.result >= 0 {
                accepted = Some(Sock(u32::try_from(c.result).unwrap()));
            } else if c.token.0 == 2 && c.result == 0 {
                connected = true;
            }
        }
        if accepted.is_some() && connected {
            break;
        }
    }
    let srv_sock = accepted.expect("accepted");
    net.submit(Token(3), Op::ArmedRecv { sock: srv_sock })
        .unwrap();

    // Arm mailbox AwaitWake on Core 1 (disk).
    io_rx.arm(&mut disk).unwrap();

    let mut rx_accum = Vec::<u8>::new();
    let mut results = Vec::new();
    for key in 0..4u32 {
        let mut frame = [0u8; 16];
        frame[0] = 1; // PUT
        frame[4..8].copy_from_slice(&key.to_le_bytes());
        frame[8..16].copy_from_slice(&(0xA000_0000 + u64::from(key)).to_le_bytes());
        write_pool_bytes(&mut net, BufId(0), 0, &frame);
        let mut sent = 0usize;
        let mut put_fut = None;
        let mut put_done = false;
        let try_dispatch = |accum: &mut Vec<u8>, pf: &mut Option<_>, task_cx: &mut Context<'_>| {
            if pf.is_none() && accum.len() >= 16 {
                let fb: [u8; 16] = accum[..16].try_into().unwrap();
                let _: std::vec::Drain<'_, u8> = accum.drain(..16);
                let k = u32::from_le_bytes(fb[4..8].try_into().unwrap());
                let v = u64::from_le_bytes(fb[8..16].try_into().unwrap());
                let mut f = Box::pin(tx.call(
                    move |slot| DiskReq::Put {
                        key: k,
                        val: v,
                        slot,
                    },
                    DiskResp::PutOk(-1),
                ));
                assert!(f.as_mut().poll(task_cx).is_pending());
                *pf = Some(f);
            }
        };

        while sent < frame.len() {
            net.submit(
                Token(10 + u64::from(key) * 10 + sent as u64),
                Op::Send {
                    sock: CLIENT_SOCK,
                    buf: BufId(0),
                    off: sent as u32,
                    len: (frame.len() - sent) as u32,
                    zero_copy: false,
                },
            )
            .unwrap();
            let mut got_send = false;
            while !got_send {
                net.tick(false, &mut net_out).unwrap();
                while let Some(c) = net_out.pop() {
                    if c.token.0 >= 10 && c.token.0 < 900 && c.result > 0 {
                        sent += usize::try_from(c.result).unwrap();
                        got_send = true;
                    } else if c.token.0 == 3 && c.result > 0 {
                        if let Some(bid) = c.buf {
                            rx_accum.extend_from_slice(
                                &net.buf(bid)[..usize::try_from(c.result).unwrap()],
                            );
                            net.give_back(bid);
                        }
                    }
                }
                try_dispatch(&mut rx_accum, &mut put_fut, &mut cx);
            }
        }
        for _ in 0..200 {
            net.tick(false, &mut net_out).unwrap();
            while let Some(c) = net_out.pop() {
                if c.token.0 == 3 && c.result > 0 {
                    if let Some(bid) = c.buf {
                        rx_accum
                            .extend_from_slice(&net.buf(bid)[..usize::try_from(c.result).unwrap()]);
                        net.give_back(bid);
                    }
                }
            }
            try_dispatch(&mut rx_accum, &mut put_fut, &mut cx);
            disk.tick(false, &mut disk_out).unwrap();
            while let Some(c) = disk_out.pop() {
                if io_rx.on_completion(&mut disk, c).unwrap() {
                    drain_disk(&mut disk, &dlim, &mut disk_out, &mut io_rx);
                }
            }
            if let Some(f) = put_fut.as_mut() {
                if let Poll::Ready(res) = f.as_mut().poll(&mut cx) {
                    assert!(matches!(res, DiskResp::PutOk(_)));
                    put_done = true;
                    break;
                }
            }
        }
        assert!(put_done, "split PUT frame must reassemble and complete");

        let mut get_fut = pin!(tx.call(|slot| DiskReq::Get { key, slot }, DiskResp::GetOk(-1, 0)));
        assert!(matches!(get_fut.as_mut().poll(&mut cx), Poll::Pending));
        let mut get_resp = None;
        for _ in 0..50 {
            disk.tick(false, &mut disk_out).unwrap();
            while let Some(c) = disk_out.pop() {
                if io_rx.on_completion(&mut disk, c).unwrap() {
                    drain_disk(&mut disk, &dlim, &mut disk_out, &mut io_rx);
                }
            }
            if let Poll::Ready(resp) = get_fut.as_mut().poll(&mut cx) {
                get_resp = Some(resp);
                break;
            }
        }
        results.push((key, get_resp.expect("get completed")));
    }

    (net.trace_hash(), disk.trace_hash(), results)
}

#[test]
fn t4_02_partitioned_two_core_storage_service_deterministic_replay() {
    let run1 = run_partitioned_service(11, 22);
    let run2 = run_partitioned_service(11, 22);
    assert_eq!(
        run1.0, run2.0,
        "Core 0 network trace hash must replay identically"
    );
    assert_eq!(
        run1.1, run2.1,
        "Core 1 disk trace hash must replay identically"
    );
    assert_eq!(run1.2, run2.2, "KV responses must replay identically");
    assert_eq!(run1.2.len(), 4);
}
