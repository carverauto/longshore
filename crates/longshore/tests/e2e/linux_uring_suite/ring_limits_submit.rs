// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Real-kernel `UringIo` `SUBMIT_ALL`, `> 16,384` registered buffers, and
//! `RLIMIT_MEMLOCK` / `RLIMIT_NOFILE` child process refusal checks.

use super::super::harness::{
    drive_until, read_pool_into, submit_and_wait_one, write_nul_path, write_pool_bytes, TempDir,
};
use longshore::io::uring::UringIo;
use longshore::io::{
    BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ENOENT,
};
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::Command;

const RLIMIT_NOFILE: i32 = 7;
const RLIMIT_MEMLOCK: i32 = 8;

#[repr(C)]
struct Rlimit {
    rlim_cur: u64,
    rlim_max: u64,
}

extern "C" {
    fn geteuid() -> u32;
    fn getrlimit(resource: i32, rlim: *mut Rlimit) -> i32;
    fn setrlimit(resource: i32, rlim: *const Rlimit) -> i32;
}

#[test]
fn uring_submit_all_partial_refusal_batch_executes_trailing_ops() {
    let tmp = TempDir::new("uring_submit_all").unwrap();
    let lim = Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 4,
        small_buffer_size: 256,
        files_max: 4,
        ops_max: 16,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(16);

    let bad_buf = lim.small(0);
    let good_buf = lim.small(1);
    let bad_path = tmp.file_path("does_not_exist/nested/missing.dat");
    let good_path = tmp.file_path("good_file.dat");
    let bad_len = write_nul_path(&mut ring, bad_buf, 0, bad_path.to_str().unwrap());
    let good_len = write_nul_path(&mut ring, good_buf, 0, good_path.to_str().unwrap());

    ring.submit(
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: bad_buf,
            path_off: 0,
            path_len: bad_len,
            flags: OpenFlags {
                read: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    ring.submit(Token(2), Op::Timeout { nanos: 100_000 })
        .unwrap();
    ring.submit(
        Token(3),
        Op::FileOpen {
            slot: FileSlot(1),
            path_buf: good_buf,
            path_off: 0,
            path_len: good_len,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        },
    )
    .unwrap();

    let cs = drive_until(&mut ring, &mut out, 200, |v| {
        v.iter().any(|c| c.token.0 == 1)
            && v.iter().any(|c| c.token.0 == 2)
            && v.iter().any(|c| c.token.0 == 3)
    })
    .unwrap();

    assert_eq!(cs.iter().find(|c| c.token.0 == 1).unwrap().result, -ENOENT);
    assert_eq!(cs.iter().find(|c| c.token.0 == 2).unwrap().result, 0);
    assert_eq!(cs.iter().find(|c| c.token.0 == 3).unwrap().result, 0);

    let _: Completion = submit_and_wait_one(
        &mut ring,
        &mut out,
        Token(4),
        Op::FileClose { slot: FileSlot(1) },
        200,
    )
    .unwrap();
}

#[test]
fn uring_more_than_16384_registered_buffers_and_last_buf_io() {
    let tmp = TempDir::new("uring_17k_bufs").unwrap();
    let lim = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 4096,
        small_buffers: 17_000,
        small_buffer_size: 256,
        files_max: 2,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut ring = UringIo::new(lim).unwrap();
    let mut out = Completions::new(8);

    let path_buf = lim.small(0);
    let last_buf = lim.small(16_999);
    assert_eq!(last_buf, BufId(17_000));
    let fpath = tmp.file_path("many_bufs.bin");
    let plen = write_nul_path(&mut ring, path_buf, 0, fpath.to_str().unwrap());

    assert_eq!(
        submit_and_wait_one(
            &mut ring,
            &mut out,
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf,
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    empty: true,
                    ..Default::default()
                },
            },
            200,
        )
        .unwrap()
        .result,
        0
    );

    let payload = [0x6Bu8; 128];
    write_pool_bytes(&mut ring, last_buf, 0, &payload);
    assert_eq!(
        submit_and_wait_one(
            &mut ring,
            &mut out,
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: last_buf,
                off: 0,
                len: 128,
                file_off: 0,
            },
            200,
        )
        .unwrap()
        .result,
        128
    );

    assert_eq!(
        submit_and_wait_one(
            &mut ring,
            &mut out,
            Token(3),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(0),
                off: 0,
                len: 128,
                file_off: 0,
            },
            200,
        )
        .unwrap()
        .result,
        128
    );
    let mut got = [0u8; 128];
    read_pool_into(&mut ring, BufId(0), 0, &mut got);
    assert_eq!(got, payload);
}

#[test]
fn uring_rlimit_memlock_refusal_e2e() {
    if std::env::var_os("LONGSHORE_E2E_URING_CHILD").as_deref() == Some(OsStr::new("memlock")) {
        let lim = Limits {
            sockets_max: 2,
            buffers: 4,
            buffer_size: 1024 * 1024,
            ops_max: 4,
            ..Limits::BASE
        };
        let err = match UringIo::new(lim) {
            Ok(_) => panic!("expected UringIo::new to fail under lowered RLIMIT_MEMLOCK"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("RLIMIT_MEMLOCK") || msg.contains("locked"),
            "error must describe RLIMIT_MEMLOCK and pool size, got: {msg}"
        );
        return;
    }

    let mut cur = Rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(unsafe { getrlimit(RLIMIT_MEMLOCK, &mut cur) }, 0);
    if unsafe { geteuid() } == 0 {
        return;
    }

    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.arg("uring_rlimit_memlock_refusal_e2e")
        .arg("--nocapture")
        .env("LONGSHORE_E2E_URING_CHILD", "memlock");
    unsafe {
        cmd.pre_exec(|| {
            let lim = Rlimit {
                rlim_cur: 64 * 1024,
                rlim_max: 64 * 1024,
            };
            if setrlimit(RLIMIT_MEMLOCK, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "child memlock check failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn uring_rlimit_nofile_refusal_e2e() {
    if std::env::var_os("LONGSHORE_E2E_URING_CHILD").as_deref() == Some(OsStr::new("nofile")) {
        let lim = Limits {
            sockets_max: 32,
            files_max: 32,
            buffers: 1,
            buffer_size: 4096,
            ops_max: 4,
            ..Limits::BASE
        };
        let err = match UringIo::new(lim) {
            Ok(_) => panic!("expected UringIo::new to fail when sockets+files > RLIMIT_NOFILE"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("64") && (msg.contains("RLIMIT_NOFILE") || msg.contains("open")),
            "error must name sockets_max + files_max and limit, got: {msg}"
        );
        return;
    }

    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.arg("uring_rlimit_nofile_refusal_e2e")
        .arg("--nocapture")
        .env("LONGSHORE_E2E_URING_CHILD", "nofile");
    unsafe {
        cmd.pre_exec(|| {
            let lim = Rlimit {
                rlim_cur: 32,
                rlim_max: 32,
            };
            if setrlimit(RLIMIT_NOFILE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "child nofile check failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
