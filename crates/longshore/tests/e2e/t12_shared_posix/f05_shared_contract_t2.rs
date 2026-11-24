// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 5: shared `Io` contract suite & `test-linux.sh` (`#41`).

use super::f05_shared_contract::{for_each_backend, workspace_root};
#[cfg(unix)]
use crate::e2e::harness::posix_backend;
#[cfg(unix)]
use crate::e2e::harness::unused_loopback_addr;
use crate::e2e::harness::{drive_until, submit_and_wait_one, write_pool_bytes};
use longshore::io::contract;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{
    BufId, Completion, Completions, Io, Limits, Op, Sock, Token, EAGAIN, ECONNREFUSED,
};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::net::SocketAddr;
use std::process::Command;

#[test]
fn t2_f05_1_test_linux_sh_refuses_a_machine_without_a_destination() {
    let root = workspace_root();
    // A Rust target and nothing after it: refused before anything is built.
    let out = Command::new("sh")
        .arg(root.join("scripts/test-linux.sh"))
        .arg("x86_64-unknown-linux-musl")
        .output()
        .expect("run test-linux.sh");
    assert!(
        !out.status.success(),
        "test-linux.sh must fail when a machine has no SSH destination"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("names no SSH destination") && stderr.contains("not exercised"),
        "expected descriptive error, got: {stderr}"
    );
}

#[test]
fn t2_f05_2_test_linux_sh_refuses_an_option_it_does_not_know() {
    let root = workspace_root();
    let out = Command::new("sh")
        .arg(root.join("scripts/test-linux.sh"))
        .arg("--no-such-option")
        .output()
        .expect("run test-linux.sh");
    assert!(
        !out.status.success(),
        "test-linux.sh must fail on an unknown option"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown option"),
        "expected unknown-option diagnostic, got: {stderr}"
    );
}

#[test]
fn t2_f05_3_connect_unbound_port_refused_and_slot_free() {
    for_each_backend(|make, addr| {
        let limits = Limits::BASE
            .with_sockets_max(4)
            .with_buffers(2)
            .with_buffer_size(256)
            .with_ops_max(4);
        let mut io = make(limits);
        let mut out = Completions::new(8);
        #[cfg(unix)]
        let ref_addr = if addr.port() == 0 {
            unused_loopback_addr().expect("addr")
        } else {
            addr
        };
        #[cfg(not(unix))]
        let ref_addr = addr;
        let c = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token::new(1),
            Op::Connect {
                sock: Sock::new(1),
                addr: ref_addr,
            },
            1_000,
        )
        .unwrap();
        assert_eq!(c.result(), -ECONNREFUSED);

        // Slot Sock(1) must be free without Close: bind listener and connect immediately.
        let bound = io.listen(Sock::new(0), addr).expect("listen");
        io.submit(
            Token::new(2),
            Op::Accept {
                listener: Sock::new(0),
                sock: Sock::new(2),
            },
        )
        .unwrap();
        io.submit(
            Token::new(3),
            Op::Connect {
                sock: Sock::new(1),
                addr: bound,
            },
        )
        .unwrap();
        let cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        assert!(cs.iter().all(|x| x.result() == 0), "reuse failed: {cs:?}");
        for (k, sock) in [Sock::new(1), Sock::new(2), Sock::new(0)]
            .into_iter()
            .enumerate()
        {
            io.submit(Token::new(10 + k as u64), Op::Close { sock })
                .unwrap();
        }
        // Teardown: the checks are done, and a slow close does not change them.
        let _: Option<Vec<longshore::io::Completion>> =
            drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 3).ok();
    });
}

#[test]
fn t2_f05_4_send_to_closed_peer_fails_without_sigpipe() {
    #[cfg(feature = "sim")]
    {
        let addr: SocketAddr = "192.0.2.54:443".parse().unwrap();
        contract::recv_now_and_closed_peer(&mut |l| SimIo::new(l, 9, Faults::default()), addr);
    }
    #[cfg(unix)]
    {
        let addr = "127.0.0.1:0".parse().unwrap();
        contract::recv_now_and_closed_peer(&mut |l| posix_backend(l).expect("posix"), addr);
    }
    #[cfg(target_os = "linux")]
    {
        let addr = "127.0.0.1:0".parse().unwrap();
        contract::recv_now_and_closed_peer(&mut |l| UringIo::new(l).expect("uring"), addr);
    }
}

/// Accepts `Sock(1)` on a listener at `addr` and connects `Sock(2)` to it.
fn accept_and_connect(io: &mut dyn Io, out: &mut Completions, addr: SocketAddr) {
    let bound = io.listen(Sock::new(0), addr).unwrap();
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(2),
            addr: bound,
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(io, out, 1_000, |v| v.len() == 2).unwrap();
}

/// One `RecvNow` of up to 64 bytes on the accepted `Sock(1)` into `BufId(0)`.
fn recv_now(io: &mut dyn Io, out: &mut Completions, tok: u64) -> Completion {
    submit_and_wait_one(
        io,
        out,
        Token::new(tok),
        Op::RecvNow {
            sock: Sock::new(1),
            buf: BufId::new(0),
            off: 0,
            len: 64,
        },
        100,
    )
    .unwrap()
}

/// Sends `msg` from `Sock(2)` through `BufId(1)` and checks it went out whole.
fn send_from_client(io: &mut dyn Io, out: &mut Completions, msg: &[u8]) {
    write_pool_bytes(io, BufId::new(1), 0, msg);
    let s = submit_and_wait_one(
        io,
        out,
        Token::new(4),
        Op::Send {
            sock: Sock::new(2),
            buf: BufId::new(1),
            off: 0,
            len: msg.len() as u32,
            zero_copy: false,
        },
        100,
    )
    .unwrap();
    assert_eq!(s.result(), i32::try_from(msg.len()).unwrap());
}

/// Polls `RecvNow` until `len` bytes arrived, skipping `EAGAIN`.
fn recv_now_all(io: &mut dyn Io, out: &mut Completions, len: usize, tok: &mut u64) -> Vec<u8> {
    let mut got = Vec::new();
    while got.len() < len {
        let r = recv_now(io, out, *tok);
        *tok += 1;
        if r.result() == -EAGAIN {
            continue;
        }
        assert!(r.result() > 0);
        got.extend_from_slice(&io.buf(BufId::new(0))[..usize::try_from(r.result()).unwrap()]);
    }
    got
}

/// Closes the client and polls `RecvNow` (with short sleeps) for the first
/// non-`EAGAIN` result, which it returns.
fn close_client_and_poll_eof(io: &mut dyn Io, out: &mut Completions, tok: &mut u64) -> i32 {
    let _: Completion = submit_and_wait_one(
        io,
        out,
        Token::new(*tok),
        Op::Close { sock: Sock::new(2) },
        100,
    )
    .unwrap();
    *tok += 1;
    for _ in 0..64 {
        let r = recv_now(io, out, *tok);
        *tok += 1;
        if r.result() != -EAGAIN {
            return r.result();
        }
        let _: Completion = submit_and_wait_one(
            io,
            out,
            Token::new(*tok),
            Op::Timeout { nanos: 500_000 },
            100,
        )
        .unwrap();
        *tok += 1;
    }
    -EAGAIN
}

#[test]
fn t2_f05_5_recv_now_eagain_bytes_and_eof() {
    for_each_backend(|make, addr| {
        let limits = Limits::BASE
            .with_sockets_max(4)
            .with_buffers(2)
            .with_buffer_size(256)
            .with_ops_max(4);
        let mut io = make(limits);
        let mut out = Completions::new(8);
        accept_and_connect(&mut *io, &mut out, addr);

        let idle = recv_now(&mut *io, &mut out, 3);
        assert_eq!(idle.result(), -EAGAIN);

        let msg = b"hello-world";
        send_from_client(&mut *io, &mut out, msg);
        let mut tok = 10u64;
        let got = recv_now_all(&mut *io, &mut out, msg.len(), &mut tok);
        assert_eq!(got.as_slice(), msg);

        let eof = close_client_and_poll_eof(&mut *io, &mut out, &mut tok);
        assert_eq!(eof, 0);

        for (k, sock) in [Sock::new(1), Sock::new(0)].into_iter().enumerate() {
            io.submit(Token::new(200 + k as u64), Op::Close { sock })
                .unwrap();
        }
        // Teardown: the checks are done, and a slow close does not change them.
        let _: Option<Vec<Completion>> =
            drive_until(&mut *io, &mut out, 100, |v| v.len() == 2).ok();
    });
}
