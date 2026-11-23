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
use crate::e2e::harness::{drive_until, submit_and_wait_one, write_pool_bytes, TempDir};
use longshore::io::contract;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, Limits, Op, Sock, Token, EAGAIN, ECONNREFUSED};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(feature = "sim")]
use std::net::SocketAddr;
use std::process::Command;

#[test]
fn t2_f05_1_test_linux_sh_fails_when_hosts_file_missing() {
    let root = workspace_root();
    let missing = root.join("nonexistent_test_hosts_e2e_check");
    let out = Command::new("sh")
        .arg(root.join("scripts/test-linux.sh"))
        .env("LONGSHORE_TEST_HOSTS_FILE", &missing)
        .output()
        .expect("run test-linux.sh");
    assert!(
        !out.status.success(),
        "test-linux.sh must fail when hosts file is missing"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("not exercised"),
        "expected descriptive error, got: {stderr}"
    );
}

#[test]
fn t2_f05_2_test_linux_sh_fails_when_hosts_file_empty_or_comments() {
    let root = workspace_root();
    let dir = TempDir::new("empty_hosts").expect("tempdir");
    let hosts = dir.file_path("hosts");
    std::fs::write(&hosts, "# comment only\n\n   # trailing comment\n").expect("write hosts");

    let out = Command::new("sh")
        .arg(root.join("scripts/test-linux.sh"))
        .env("LONGSHORE_TEST_HOSTS_FILE", &hosts)
        .output()
        .expect("run test-linux.sh");
    assert!(
        !out.status.success(),
        "test-linux.sh must fail when hosts file has 0 hosts"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("0 hosts") || stderr.contains("not exercised"),
        "expected 0 hosts diagnostic, got: {stderr}"
    );
}

#[test]
fn t2_f05_3_connect_unbound_port_refused_and_slot_free() {
    for_each_backend(|make, addr| {
        let limits = Limits {
            sockets_max: 4,
            buffers: 2,
            buffer_size: 256,
            ops_max: 4,
            ..Limits::BASE
        };
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
            Token(1),
            Op::Connect {
                sock: Sock(1),
                addr: ref_addr,
            },
            1_000,
        )
        .unwrap();
        assert_eq!(c.result, -ECONNREFUSED);

        // Slot Sock(1) must be free without Close: bind listener and connect immediately.
        let bound = io.listen(Sock(0), addr).expect("listen");
        io.submit(
            Token(2),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
        io.submit(
            Token(3),
            Op::Connect {
                sock: Sock(1),
                addr: bound,
            },
        )
        .unwrap();
        let cs = drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();
        assert!(cs.iter().all(|x| x.result == 0), "reuse failed: {cs:?}");
        for (k, sock) in [Sock(1), Sock(2), Sock(0)].into_iter().enumerate() {
            io.submit(Token(10 + k as u64), Op::Close { sock }).unwrap();
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

#[test]
fn t2_f05_5_recv_now_eagain_bytes_and_eof() {
    for_each_backend(|make, addr| {
        let limits = Limits {
            sockets_max: 4,
            buffers: 2,
            buffer_size: 256,
            ops_max: 4,
            ..Limits::BASE
        };
        let mut io = make(limits);
        let mut out = Completions::new(8);
        let bound = io.listen(Sock(0), addr).unwrap();
        io.submit(
            Token(1),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(1),
            },
        )
        .unwrap();
        io.submit(
            Token(2),
            Op::Connect {
                sock: Sock(2),
                addr: bound,
            },
        )
        .unwrap();
        let _: Vec<longshore::io::Completion> =
            drive_until(&mut *io, &mut out, 1_000, |v| v.len() == 2).unwrap();

        let idle = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(3),
            Op::RecvNow {
                sock: Sock(1),
                buf: BufId(0),
                off: 0,
                len: 64,
            },
            100,
        )
        .unwrap();
        assert_eq!(idle.result, -EAGAIN);

        let msg = b"hello-world";
        write_pool_bytes(&mut *io, BufId(1), 0, msg);
        let s = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(4),
            Op::Send {
                sock: Sock(2),
                buf: BufId(1),
                off: 0,
                len: msg.len() as u32,
                zero_copy: false,
            },
            100,
        )
        .unwrap();
        assert_eq!(s.result, msg.len() as i32);

        let mut got = Vec::new();
        let mut tok = 10u64;
        while got.len() < msg.len() {
            let r = submit_and_wait_one(
                &mut *io,
                &mut out,
                Token(tok),
                Op::RecvNow {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: 64,
                },
                100,
            )
            .unwrap();
            tok += 1;
            if r.result == -EAGAIN {
                continue;
            }
            assert!(r.result > 0);
            got.extend_from_slice(&io.buf(BufId(0))[..r.result as usize]);
        }
        assert_eq!(got.as_slice(), msg);

        let _: longshore::io::Completion = submit_and_wait_one(
            &mut *io,
            &mut out,
            Token(tok),
            Op::Close { sock: Sock(2) },
            100,
        )
        .unwrap();
        tok += 1;
        let mut eof = -EAGAIN;
        for _ in 0..64 {
            let r = submit_and_wait_one(
                &mut *io,
                &mut out,
                Token(tok),
                Op::RecvNow {
                    sock: Sock(1),
                    buf: BufId(0),
                    off: 0,
                    len: 64,
                },
                100,
            )
            .unwrap();
            tok += 1;
            if r.result != -EAGAIN {
                eof = r.result;
                break;
            }
            let _: longshore::io::Completion = submit_and_wait_one(
                &mut *io,
                &mut out,
                Token(tok),
                Op::Timeout { nanos: 500_000 },
                100,
            )
            .unwrap();
            tok += 1;
        }
        assert_eq!(eof, 0);

        for (k, sock) in [Sock(1), Sock(0)].into_iter().enumerate() {
            io.submit(Token(200 + k as u64), Op::Close { sock })
                .unwrap();
        }
        // Teardown: the checks are done, and a slow close does not change them.
        let _: Option<Vec<longshore::io::Completion>> =
            drive_until(&mut *io, &mut out, 100, |v| v.len() == 2).ok();
    });
}
