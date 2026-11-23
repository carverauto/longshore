// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Kernel checks for `file_uring`: contracts, `O_DIRECT`, `SUBMIT_ALL`, `>16384` bufs, `getrlimit`.

use crate::die::OrDie;
use longshore::io::alloc_guard::sealed;
use longshore::io::posix::PosixIo;
use longshore::io::uring::UringIo;
use longshore::io::{
    contract_chains, contract_files, BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags,
    Token, EINVAL, ENOENT,
};
use std::path::Path;

#[path = "limits.rs"]
mod limits;

pub(super) fn run_memlock_child() {
    limits::run_memlock_child();
}

pub(super) fn run_all(dir: &Path, posix: bool) {
    let prefix = format!("{}/", dir.display());
    contract_files::run_all(&mut |l| UringIo::new(l).or_die("UringIo::new"), &prefix);
    contract_chains::run_all(&mut |l| UringIo::new(l).or_die("UringIo::new"), &prefix);
    if posix {
        let p_dir = dir.join("posix");
        std::fs::create_dir_all(&p_dir).or_die("create posix dir");
        let p_prefix = format!("{}/", p_dir.display());
        contract_files::run_all(&mut |l| PosixIo::new(l).or_die("PosixIo::new"), &p_prefix);
        contract_chains::run_all(&mut |l| PosixIo::new(l).or_die("PosixIo::new"), &p_prefix);
    }
    check_sealed_o_direct(dir);
    check_submit_all_partial_refusal(dir);
    check_over_16384_buffers(dir);
    limits::check_rlimit_nofile();
    limits::check_memlock_in_child();
}

fn check_sealed_o_direct(dir: &Path) {
    let limits = Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 4096,
        files_max: 2,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).or_die("UringIo::new");
    let mut out = Completions::new(8);
    let path = format!("{}/sealed_direct.bin\0", dir.display());
    io.buf(BufId(0))[..path.len()].copy_from_slice(path.as_bytes());
    let wbuf = limits.aligned(0);
    let rbuf = limits.aligned(1);
    for (i, b) in io.buf(wbuf).iter_mut().enumerate() {
        *b = (i.wrapping_mul(29) ^ 0xA5) as u8;
    }

    // Verify misaligned O_DIRECT write is refused with -EINVAL and valid chain succeeds under sealed.
    sealed(|| {
        io.submit_chain(&[
            (
                Token(1),
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: path.len() as u32,
                    flags: OpenFlags {
                        read: true,
                        write: true,
                        create: true,
                        direct: true,
                        ..Default::default()
                    },
                },
            ),
            (
                Token(2),
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: wbuf,
                    off: 0,
                    len: 4096,
                    file_off: 0,
                },
            ),
            (
                Token(3),
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: true,
                },
            ),
        ])
        .unwrap();
        let mut done = 0;
        while done < 3 {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == Token(1) {
                    assert_eq!(c.result, 0, "O_DIRECT open failed on {}", dir.display());
                } else if c.token == Token(2) {
                    assert_eq!(c.result, 4096);
                } else {
                    assert_eq!(c.result, 0);
                }
                done += 1;
            }
        }
        io.submit(
            Token(4),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: wbuf,
                off: 0,
                len: 512,
                file_off: 0,
            },
        )
        .unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, -EINVAL);

        io.submit(
            Token(5),
            Op::FileRead {
                slot: FileSlot(0),
                buf: rbuf,
                off: 0,
                len: 4096,
                file_off: 0,
            },
        )
        .unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, 4096);

        io.submit(Token(6), Op::FileClose { slot: FileSlot(0) })
            .unwrap();
        io.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, 0);
    });
    let (wb, rb) = io.buf_pair(wbuf, rbuf);
    assert_eq!(wb, rb);
}

fn check_submit_all_partial_refusal(dir: &Path) {
    let limits = Limits {
        buffers: 2,
        buffer_size: 4096,
        files_max: 2,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).or_die("UringIo::new");
    let mut out = Completions::new(8);
    let bad = format!("{}/no_such_dir_xyz/bad.bin\0", dir.display());
    let good = format!("{}/after_bad.bin\0", dir.display());
    io.buf(BufId(0))[..bad.len()].copy_from_slice(bad.as_bytes());
    io.buf(BufId(1))[..good.len()].copy_from_slice(good.as_bytes());

    // Submit a kernel-refused open ahead of a valid open in the same batch.
    io.submit(
        Token(10),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: bad.len() as u32,
            flags: OpenFlags {
                read: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    io.submit(
        Token(11),
        Op::FileOpen {
            slot: FileSlot(1),
            path_buf: BufId(1),
            path_off: 0,
            path_len: good.len() as u32,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    let mut r10 = None;
    let mut r11 = None;
    for _ in 0..1000 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(10) {
                r10 = Some(c.result);
            } else if c.token == Token(11) {
                r11 = Some(c.result);
            }
        }
        if r10.is_some() && r11.is_some() {
            break;
        }
    }
    assert_eq!(r10, Some(-ENOENT));
    assert_eq!(
        r11,
        Some(0),
        "SUBMIT_ALL must deliver op queued behind failure"
    );
    io.submit(Token(12), Op::FileClose { slot: FileSlot(1) })
        .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);
}

fn check_over_16384_buffers(dir: &Path) {
    let limits = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 4096,
        small_buffers: 16_400,
        small_buffer_size: 256,
        files_max: 2,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = UringIo::new(limits).or_die("UringIo::new with 16401 buffers");
    let mut out = Completions::new(8);
    let path = format!("{}/many_bufs.bin\0", dir.display());
    io.buf(BufId(0))[..path.len()].copy_from_slice(path.as_bytes());
    io.submit(
        Token(1),
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: path.len() as u32,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);

    let last = limits.small(16_399);
    io.buf(last)[..64].fill(0x5E);
    io.submit(
        Token(2),
        Op::FileWrite {
            slot: FileSlot(0),
            buf: last,
            off: 0,
            len: 64,
            file_off: 0,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 64);

    io.submit(
        Token(3),
        Op::FileRead {
            slot: FileSlot(0),
            buf: BufId(0),
            off: 0,
            len: 64,
            file_off: 0,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 64);
    assert!(io.buf(BufId(0))[..64].iter().all(|&b| b == 0x5E));

    io.submit(Token(4), Op::FileClose { slot: FileSlot(0) })
        .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);
}
