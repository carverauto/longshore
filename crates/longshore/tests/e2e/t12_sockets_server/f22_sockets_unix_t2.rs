// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 22 Tier 2 E2E tests: Unix error paths, option failures & close races.

#[cfg(unix)]
use super::super::harness::{
    drive_until, posix_backend, write_nul_path, write_pool_bytes, TempDir,
};
#[cfg(any(feature = "sim", unix))]
use super::f22_sockets_unix_t1::unix_limits;
#[cfg(feature = "sim")]
use super::f22_sockets_unix_t1::SIM_ADDR;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
#[cfg(any(feature = "sim", unix))]
use longshore::io::{BufId, Completions, Io, Op, Sock, Token};
#[cfg(unix)]
use longshore::io::{ECONNREFUSED, ENOENT};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

#[cfg(unix)]
const EINVAL: i32 = 22;
#[cfg(unix)]
const ENAMETOOLONG: i32 = 36;
#[cfg(target_os = "linux")]
const EPERM: i32 = 1;
#[cfg(target_os = "linux")]
const ENOSYS: i32 = 38;

#[cfg(unix)]
#[test]
fn t2_f22_1_unix_connect_without_listener_fails_and_frees_slot() {
    let dir = TempDir::new("unix_nolisten").expect("tempdir");
    let missing = dir.socket_path("missing.sock");
    let live = dir.socket_path("live.sock");
    let mut posix = posix_backend(unix_limits()).expect("posix");
    let mut out = Completions::new(16);

    let plen = write_nul_path(&mut posix, BufId(0), 0, missing.to_str().unwrap());
    posix
        .submit(
            Token(1),
            Op::ConnectUnix {
                sock: Sock(1),
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 1).unwrap();
    assert!(
        cs[0].result == -ENOENT || cs[0].result == -ECONNREFUSED,
        "unexpected error: {}",
        cs[0].result
    );

    posix.listen_unix(Sock(0), live.to_str().unwrap()).unwrap();
    let plen2 = write_nul_path(&mut posix, BufId(0), 0, live.to_str().unwrap());
    posix
        .submit(
            Token(2),
            Op::Accept {
                listener: Sock(0),
                sock: Sock(2),
            },
        )
        .unwrap();
    posix
        .submit(
            Token(3),
            Op::ConnectUnix {
                sock: Sock(1),
                path_buf: BufId(0),
                path_off: 0,
                path_len: plen2,
            },
        )
        .unwrap();
    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 2).unwrap();
    for c in cs {
        assert_eq!(c.result, 0);
    }
}

#[cfg(unix)]
#[test]
fn t2_f22_2_unix_connect_invalid_nul_or_overlong_path_rejected() {
    let mut posix = posix_backend(unix_limits()).expect("posix");
    let mut out = Completions::new(16);

    write_pool_bytes(&mut posix, BufId(0), 0, b"/tmp/no_nul_path");
    posix
        .submit(
            Token(1),
            Op::ConnectUnix {
                sock: Sock(1),
                path_buf: BufId(0),
                path_off: 0,
                path_len: 16,
            },
        )
        .unwrap();
    write_pool_bytes(&mut posix, BufId(1), 0, b"/tmp/a\0b\0");
    posix
        .submit(
            Token(2),
            Op::ConnectUnix {
                sock: Sock(2),
                path_buf: BufId(1),
                path_off: 0,
                path_len: 9,
            },
        )
        .unwrap();
    let mut overlong = [b'a'; 120];
    overlong[119] = 0;
    write_pool_bytes(&mut posix, BufId(2), 0, &overlong);
    posix
        .submit(
            Token(3),
            Op::ConnectUnix {
                sock: Sock(3),
                path_buf: BufId(2),
                path_off: 0,
                path_len: 120,
            },
        )
        .unwrap();

    let cs = drive_until(&mut posix, &mut out, 50, |v| v.len() == 3).unwrap();
    for c in cs {
        assert!(
            c.result == -EINVAL || c.result == -ENAMETOOLONG,
            "expected EINVAL/ENAMETOOLONG, got {c:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn t2_f22_3_unix_listen_existing_path_or_bad_parent_returns_error() {
    let dir = TempDir::new("unix_badlisten").expect("tempdir");
    let existing = dir.socket_path("taken.sock");
    std::fs::write(&existing, b"occupied").unwrap();
    let mut posix = posix_backend(unix_limits()).expect("posix");
    assert!(posix
        .listen_unix(Sock(0), existing.to_str().unwrap())
        .is_err());

    let bad_parent = dir.path().join("nonexistent_dir").join("s.sock");
    assert!(posix
        .listen_unix(Sock(1), bad_parent.to_str().unwrap())
        .is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn t2_f22_4_uring_socket_options_connect_and_accept_without_silent_fallback() {
    let mut io = match UringIo::new(unix_limits()) {
        Ok(u) => u,
        Err(e) if e.raw_os_error() == Some(EPERM) || e.raw_os_error() == Some(ENOSYS) => {
            return;
        }
        Err(e) => panic!("UringIo::new failed: {e}"),
    };
    io.no_delay(true);
    io.keep_alive(true);
    let mut out = Completions::new(16);
    let bound = io.listen(Sock(0), "127.0.0.1:0".parse().unwrap()).unwrap();
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
            addr: bound,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 50, |v| v.len() == 2).unwrap();
    for c in cs {
        assert_eq!(c.result, 0);
    }
}

#[cfg(feature = "sim")]
#[test]
fn t2_f22_5_close_ends_both_armed_recv_and_pending_send_and_signals_eof() {
    let mut io = SimIo::new(unix_limits(), 2215, Faults::default());
    let mut out = Completions::new(16);
    let addr = io.listen(Sock(0), SIM_ADDR).unwrap();
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
    drive_until(&mut io, &mut out, 30, |v| v.len() == 2).unwrap();

    io.submit(Token(3), Op::ArmedRecv { sock: Sock(2) })
        .unwrap();
    write_pool_bytes(&mut io, BufId(0), 0, &[0x11; 1024]);
    io.submit(
        Token(4),
        Op::Send {
            sock: Sock(2),
            buf: BufId(0),
            off: 0,
            len: 1024,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(Token(5), Op::Close { sock: Sock(2) }).unwrap();

    let cs = drive_until(&mut io, &mut out, 40, |v| {
        v.iter().any(|c| c.token == Token(3) && !c.more)
            && v.iter().any(|c| c.token == Token(4))
            && v.iter().any(|c| c.token == Token(5))
    })
    .unwrap();
    assert!(!cs.iter().find(|c| c.token == Token(3)).unwrap().more);
    assert!(!cs.iter().find(|c| c.token == Token(4)).unwrap().more);

    let mut saw_eof = false;
    for k in 0..8u64 {
        io.submit(
            Token(10 + k),
            Op::Recv {
                sock: Sock(1),
                buf: BufId(1),
                off: 0,
                len: 1024,
            },
        )
        .unwrap();
        let rc = drive_until(&mut io, &mut out, 30, |v| !v.is_empty()).unwrap();
        if rc[0].result <= 0 {
            assert_eq!(rc[0].result, 0);
            saw_eof = true;
            break;
        }
    }
    assert!(saw_eof);
}
