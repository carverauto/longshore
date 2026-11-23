// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-02`: Multi-core partitioned storage service (`2x Engine<Virtual>` +
//! `longshore::mailbox` + `submit_chain` + Direct I/O + network/disk faults).

use super::super::harness::write_pool_bytes;
use super::partitioned_disk::{DiskCore, DiskReq, DiskResp};
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use longshore::mailbox::Sender;
use longshore::sim::{Faults, SimIo};
use std::future::Future;
use std::pin::{pin, Pin};
use std::task::{Context, Poll, Waker};

const LISTENER: Sock = Sock(0);
const CLIENT_SOCK: Sock = Sock(1);
const ACCEPT_START: u32 = 2;
const ACCEPT_LEN: u32 = 4;

/// A PUT dispatched to the disk core and not yet answered.
type PutFuture<'a> = Pin<Box<dyn Future<Output = DiskResp> + 'a>>;

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

/// The network core: its simulator, completions, and reassembly buffer.
struct NetCore {
    net: SimIo,
    out: Completions,
    rx_accum: Vec<u8>,
}

impl NetCore {
    /// Appends the bytes of a server armed recv (`Token(3)`) completion and
    /// gives its ring buffer back.
    fn take_recv(&mut self, c: &Completion) {
        if c.token.0 == 3 && c.result > 0 {
            if let Some(bid) = c.buf {
                self.rx_accum
                    .extend_from_slice(&self.net.buf(bid)[..usize::try_from(c.result).unwrap()]);
                self.net.give_back(bid);
            }
        }
    }
}

/// Connects the client through an armed accept and arms recv on the server
/// side (`Token(3)`).
fn connect_net(net_seed: u64) -> NetCore {
    let net_faults = Faults {
        delay: 150_000,
        partial_recv: 200_000,
        short_send: 200_000,
        ..Faults::default()
    };
    let mut nc = NetCore {
        net: SimIo::new(net_limits(), net_seed, net_faults),
        out: Completions::new(16),
        rx_accum: Vec::new(),
    };
    let addr = nc
        .net
        .listen(LISTENER, "127.0.0.1:9100".parse().unwrap())
        .unwrap();
    nc.net
        .submit(Token(1), Op::ArmedAccept { listener: LISTENER })
        .unwrap();
    nc.net
        .submit(
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
        nc.net.tick(false, &mut nc.out).unwrap();
        while let Some(c) = nc.out.pop() {
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
    nc.net
        .submit(Token(3), Op::ArmedRecv { sock: srv_sock })
        .unwrap();
    nc
}

/// Once a whole 16-byte PUT frame has arrived and none is in flight, sends
/// it to the disk core and polls the call once.
fn try_dispatch<'a>(
    tx: &'a Sender<DiskReq, DiskResp>,
    accum: &mut Vec<u8>,
    pf: &mut Option<PutFuture<'a>>,
    task_cx: &mut Context<'_>,
) {
    if pf.is_none() && accum.len() >= 16 {
        let fb: [u8; 16] = accum[..16].try_into().unwrap();
        let _: std::vec::Drain<'_, u8> = accum.drain(..16);
        let k = u32::from_le_bytes(fb[4..8].try_into().unwrap());
        let v = u64::from_le_bytes(fb[8..16].try_into().unwrap());
        let mut f: PutFuture<'a> = Box::pin(tx.call(
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
}

/// Sends the PUT frame for `key` (possibly in several short sends),
/// reassembling and dispatching whatever arrives meanwhile.
fn send_put_frame<'a>(
    nc: &mut NetCore,
    tx: &'a Sender<DiskReq, DiskResp>,
    key: u32,
    put_fut: &mut Option<PutFuture<'a>>,
    cx: &mut Context<'_>,
) {
    let mut frame = [0u8; 16];
    frame[0] = 1; // PUT
    frame[4..8].copy_from_slice(&key.to_le_bytes());
    frame[8..16].copy_from_slice(&(0xA000_0000 + u64::from(key)).to_le_bytes());
    write_pool_bytes(&mut nc.net, BufId(0), 0, &frame);
    let mut sent = 0usize;
    while sent < frame.len() {
        nc.net
            .submit(
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
            nc.net.tick(false, &mut nc.out).unwrap();
            while let Some(c) = nc.out.pop() {
                if c.token.0 >= 10 && c.token.0 < 900 && c.result > 0 {
                    sent += usize::try_from(c.result).unwrap();
                    got_send = true;
                } else {
                    nc.take_recv(&c);
                }
            }
            try_dispatch(tx, &mut nc.rx_accum, put_fut, cx);
        }
    }
}

/// Pumps both cores until the dispatched PUT is answered; returns whether it was.
fn await_put<'a>(
    nc: &mut NetCore,
    dc: &mut DiskCore,
    tx: &'a Sender<DiskReq, DiskResp>,
    put_fut: &mut Option<PutFuture<'a>>,
    cx: &mut Context<'_>,
) -> bool {
    for _ in 0..200 {
        nc.net.tick(false, &mut nc.out).unwrap();
        while let Some(c) = nc.out.pop() {
            nc.take_recv(&c);
        }
        try_dispatch(tx, &mut nc.rx_accum, put_fut, cx);
        dc.pump();
        if let Some(f) = put_fut.as_mut() {
            if let Poll::Ready(res) = f.as_mut().poll(cx) {
                assert!(matches!(res, DiskResp::PutOk(_)));
                return true;
            }
        }
    }
    false
}

/// Reads `key` back through the disk core's mailbox.
fn get_key(
    dc: &mut DiskCore,
    tx: &Sender<DiskReq, DiskResp>,
    key: u32,
    cx: &mut Context<'_>,
) -> Option<DiskResp> {
    let mut get_fut = pin!(tx.call(|slot| DiskReq::Get { key, slot }, DiskResp::GetOk(-1, 0)));
    assert!(matches!(get_fut.as_mut().poll(cx), Poll::Pending));
    for _ in 0..50 {
        dc.pump();
        if let Poll::Ready(resp) = get_fut.as_mut().poll(cx) {
            return Some(resp);
        }
    }
    None
}

fn run_partitioned_service(net_seed: u64, disk_seed: u64) -> (u64, u64, Vec<(u32, DiskResp)>) {
    let (mut dc, tx) = DiskCore::new(disk_seed);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut nc = connect_net(net_seed);
    dc.arm();

    let mut results = Vec::new();
    for key in 0..4u32 {
        let mut put_fut = None;
        send_put_frame(&mut nc, &tx, key, &mut put_fut, &mut cx);
        let put_done = await_put(&mut nc, &mut dc, &tx, &mut put_fut, &mut cx);
        assert!(put_done, "split PUT frame must reassemble and complete");
        results.push((
            key,
            get_key(&mut dc, &tx, key, &mut cx).expect("get completed"),
        ));
    }

    (nc.net.trace_hash(), dc.disk.trace_hash(), results)
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
