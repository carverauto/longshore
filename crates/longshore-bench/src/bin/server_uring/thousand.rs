// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! 1,000-idle-connection buffer-ring check for `server_uring`.

use super::checks::{cancel_armed, close_batch};
use crate::die::OrDie;
use longshore::io::alloc_guard::sealed;
use longshore::io::{BufId, Completion, Completions, Io, Limits, Op, Sock, Token};
use std::net::SocketAddr;

const TOTAL: usize = 1000;
const HALF: usize = 500;

/// A server ring with 1,000 accepted connections from two client rings of
/// 500 each, and which of the server's receives are armed.
struct Rig<IO> {
    srv: IO,
    c0: IO,
    c1: IO,
    srv_out: Completions,
    c_out: Completions,
    accepted: Vec<Sock>,
    recv_armed: [bool; TOTAL],
}

pub(super) fn thousand_idle_connections<IO: Io>(
    make: &mut impl FnMut(Limits) -> IO,
    addr: SocketAddr,
) {
    let srv_limits = Limits {
        sockets_max: 1002,
        accept_range_start: 1,
        accept_range_len: 1000,
        buffers: 1,
        buffer_size: 256,
        ring_buffers: 16,
        ring_buffer_size: 256,
        ops_max: 1024,
        ..Limits::BASE
    };
    let cli_limits = Limits {
        sockets_max: HALF as u32,
        buffers: 2,
        buffer_size: 256,
        ops_max: 512,
        ..Limits::BASE
    };
    let mut rig = Rig {
        srv: make(srv_limits),
        c0: make(cli_limits),
        c1: make(cli_limits),
        srv_out: Completions::new(128),
        c_out: Completions::new(128),
        accepted: Vec::with_capacity(TOTAL),
        recv_armed: [false; TOTAL],
    };

    let bound = rig.srv.listen(Sock(0), addr).or_die("listen");
    let mut client_socks = Vec::with_capacity(TOTAL);
    for k in 0..HALF {
        client_socks.push((0u8, Sock(k as u32)));
    }
    for k in 0..HALF {
        client_socks.push((1u8, Sock(k as u32)));
    }

    sealed(|| {
        let accept_armed = rig.connect_all(bound, &client_socks);
        // Cancel ArmedAccept so only 1,000 armed receives remain in flight.
        if accept_armed {
            cancel_armed(&mut rig.srv, &mut rig.srv_out, Token(2), Token(1));
        }
        rig.arm_receives();
        rig.send_and_hold();
        rig.send_after_return();
        rig.close_all();
    });
}

impl<IO: Io> Rig<IO> {
    /// Connect every client socket to `bound` in batches of 64 while the
    /// server accepts them. Returns whether the armed accept is still armed.
    fn connect_all(&mut self, bound: SocketAddr, client_socks: &[(u8, Sock)]) -> bool {
        self.srv
            .submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
            .or_die("submit");
        let mut accept_armed = true;

        for batch in client_socks.chunks(64) {
            for (idx, &(which, sock)) in batch.iter().enumerate() {
                let op = Op::Connect { sock, addr: bound };
                if which == 0 {
                    self.c0
                        .submit(Token(1000 + idx as u64), op)
                        .or_die("submit");
                } else {
                    self.c1
                        .submit(Token(1000 + idx as u64), op)
                        .or_die("submit");
                }
            }
            let mut want_c = batch.len();
            let want_s = self.accepted.len() + batch.len();
            let mut spins = 0u32;
            while want_c > 0 || self.accepted.len() < want_s {
                spins += 1;
                assert!(
                    spins < 500_000,
                    "connect/accept stalled: want_c={want_c}, accepted={}/{}",
                    self.accepted.len(),
                    want_s
                );
                if !accept_armed && self.accepted.len() < want_s {
                    self.srv
                        .submit(Token(1), Op::ArmedAccept { listener: Sock(0) })
                        .or_die("submit");
                    accept_armed = true;
                }
                self.c0.tick(false, &mut self.c_out).or_die("tick");
                while let Some(c) = self.c_out.pop() {
                    assert_eq!(c.result, 0, "c0 connect: {c:?}");
                    want_c -= 1;
                }
                self.c1.tick(false, &mut self.c_out).or_die("tick");
                while let Some(c) = self.c_out.pop() {
                    assert_eq!(c.result, 0, "c1 connect: {c:?}");
                    want_c -= 1;
                }
                self.srv.tick(false, &mut self.srv_out).or_die("tick");
                while let Some(c) = self.srv_out.pop() {
                    assert!(c.token.0 == 1 && c.result >= 1, "{c:?}");
                    accept_armed = c.more;
                    self.accepted
                        .push(Sock(u32::try_from(c.result).or_die("accepted socket")));
                }
            }
        }
        assert_eq!(self.accepted.len(), TOTAL);
        accept_armed
    }

    /// Arm receive on all 1,000 idle connections; zero ring buffers must be held.
    fn arm_receives(&mut self) {
        for (k, &s) in self.accepted.iter().enumerate() {
            self.srv
                .submit(Token(10_000 + k as u64), Op::ArmedRecv { sock: s })
                .or_die("submit");
            self.recv_armed[k] = true;
        }
        self.srv.tick(false, &mut self.srv_out).or_die("tick");
        assert!(
            self.srv_out.is_empty(),
            "idle connections held a ring buffer"
        );
    }

    /// Send one byte on each of 16 sockets of `cli`, starting at `first`,
    /// with the byte `tag + k` and tokens from `token`.
    fn send16(cli: &mut IO, first: u32, tag: u32, token: u64) {
        for k in 0..16u32 {
            cli.buf(BufId(0))[0] = (tag + k) as u8;
            cli.submit(
                Token(token + u64::from(k)),
                Op::Send {
                    sock: Sock(first + k),
                    buf: BufId(0),
                    off: 0,
                    len: 1,
                    zero_copy: false,
                },
            )
            .or_die("submit");
        }
    }

    /// Note a server receive completion: one byte, and whether still armed.
    fn on_recv(&mut self, c: &Completion) -> BufId {
        let idx = (c.token.0 - 10_000) as usize;
        self.recv_armed[idx] = c.more;
        assert_eq!(c.result, 1);
        c.buf.or_die("a ring buffer in the completion")
    }

    /// Send 16 bytes across c0[0..16] proving all 16 ring buffers are free,
    /// holding every buffer until all 16 have arrived.
    fn send_and_hold(&mut self) {
        Self::send16(&mut self.c0, 0, 0, 20_000);
        let mut sent = 0;
        let mut recvd = 0;
        let mut held = [None; 16];
        let mut spins = 0u32;
        while sent < 16 || recvd < 16 {
            spins += 1;
            assert!(spins < 500_000, "send16 stalled: sent={sent} recvd={recvd}");
            self.c0.tick(false, &mut self.c_out).or_die("tick");
            while let Some(c) = self.c_out.pop() {
                assert_eq!(c.result, 1);
                sent += 1;
            }
            self.srv.tick(false, &mut self.srv_out).or_die("tick");
            while let Some(c) = self.srv_out.pop() {
                held[recvd] = Some(self.on_recv(&c));
                recvd += 1;
            }
        }
        for b in held.into_iter().flatten() {
            self.srv.give_back(b);
        }
    }

    /// Send 16 more across c1[484..500] to confirm all 16 returned and usable.
    fn send_after_return(&mut self) {
        Self::send16(&mut self.c1, 484, 0x80, 21_000);
        let (mut sent2, mut recvd2, mut spins2) = (0, 0, 0u32);
        while sent2 < 16 || recvd2 < 16 {
            spins2 += 1;
            assert!(
                spins2 < 500_000,
                "send16_2 stalled: sent2={sent2} recvd2={recvd2}"
            );
            self.c1.tick(false, &mut self.c_out).or_die("tick");
            while let Some(c) = self.c_out.pop() {
                assert_eq!(c.result, 1);
                sent2 += 1;
            }
            self.srv.tick(false, &mut self.srv_out).or_die("tick");
            while let Some(c) = self.srv_out.pop() {
                let b = self.on_recv(&c);
                self.srv.give_back(b);
                recvd2 += 1;
            }
        }
    }

    /// Close accepted sockets in batches of 16 (since 1,000/1,024 ops slots
    /// are occupied), then the listener and every client socket.
    fn close_all(&mut self) {
        for (chunk_idx, chunk) in self.accepted.chunks(16).enumerate() {
            let base = chunk_idx * 16;
            let mut need_close = chunk.len();
            let mut need_cancel = 0usize;
            for (o, &s) in chunk.iter().enumerate() {
                if self.recv_armed[base + o] {
                    need_cancel += 1;
                }
                self.srv
                    .submit(Token(90_000 + o as u64), Op::Close { sock: s })
                    .or_die("submit");
            }
            while need_close > 0 || need_cancel > 0 {
                self.srv.tick(true, &mut self.srv_out).or_die("tick");
                while let Some(c) = self.srv_out.pop() {
                    if c.token.0 >= 90_000 {
                        need_close -= 1;
                    } else if (10_000..11_000).contains(&c.token.0) && !c.more {
                        let idx = (c.token.0 - 10_000) as usize;
                        if self.recv_armed[idx] {
                            self.recv_armed[idx] = false;
                            need_cancel -= 1;
                        }
                    }
                }
            }
        }
        self.srv
            .submit(Token(99_999), Op::Close { sock: Sock(0) })
            .or_die("submit");
        self.srv.tick(true, &mut self.srv_out).or_die("tick");
        assert!(self.srv_out.pop().is_some());
        let c_socks: [Sock; HALF] = core::array::from_fn(|k| Sock(k as u32));
        close_batch(&mut self.c0, &mut self.c_out, &c_socks);
        close_batch(&mut self.c1, &mut self.c_out, &c_socks);
    }
}
