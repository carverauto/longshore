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
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_files_max(2)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_ops_max(8);
    let mut io = UringIo::new(limits).or_die("UringIo::new");
    let mut out = Completions::new(8);
    let path = format!("{}/sealed_direct.bin\0", dir.display());
    io.buf(BufId::new(0))[..path.len()].copy_from_slice(path.as_bytes());
    let wbuf = limits.aligned(0);
    let rbuf = limits.aligned(1);
    for (i, b) in io.buf(wbuf).iter_mut().enumerate() {
        *b = (i.wrapping_mul(29) ^ 0xA5) as u8;
    }

    // Verify misaligned O_DIRECT write is refused with -EINVAL and valid chain succeeds under sealed.
    sealed(|| {
        open_write_sync_chain(&mut io, &mut out, path.len() as u32, wbuf, dir);
        refuse_misaligned_then_read(&mut io, &mut out, wbuf, rbuf);
    });
    let (wb, rb) = io.buf_pair(wbuf, rbuf);
    assert_eq!(wb, rb);
}

/// Open the path in buffer 0 for direct I/O, write `wbuf` and sync, as one
/// linked chain.
fn open_write_sync_chain(
    io: &mut UringIo,
    out: &mut Completions,
    path_len: u32,
    wbuf: BufId,
    dir: &Path,
) {
    io.submit_chain(&[
        (
            Token::new(1),
            Op::FileOpen {
                slot: FileSlot::new(0),
                path_buf: BufId::new(0),
                path_off: 0,
                path_len,
                flags: OpenFlags::NONE
                    .with_read(true)
                    .with_write(true)
                    .with_create(true)
                    .with_direct(true),
            },
        ),
        (
            Token::new(2),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: wbuf,
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        (
            Token::new(3),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ])
    .or_die("submit_chain");
    let mut done = 0;
    while done < 3 {
        io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == 1 {
                assert_eq!(c.result(), 0, "O_DIRECT open failed on {}", dir.display());
            } else if c.token().get() == 2 {
                assert_eq!(c.result(), 4096);
            } else {
                assert_eq!(c.result(), 0);
            }
            done += 1;
        }
    }
}

/// A misaligned direct write is refused with `-EINVAL`; an aligned read into
/// `rbuf` succeeds; the file closes.
fn refuse_misaligned_then_read(io: &mut UringIo, out: &mut Completions, wbuf: BufId, rbuf: BufId) {
    io.submit(
        Token::new(4),
        Op::FileWrite {
            slot: FileSlot::new(0),
            buf: wbuf,
            off: 0,
            len: 512,
            file_off: 0,
        },
    )
    .or_die("submit");
    io.tick(true, out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), -EINVAL);

    io.submit(
        Token::new(5),
        Op::FileRead {
            slot: FileSlot::new(0),
            buf: rbuf,
            off: 0,
            len: 4096,
            file_off: 0,
        },
    )
    .or_die("submit");
    io.tick(true, out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 4096);

    io.submit(
        Token::new(6),
        Op::FileClose {
            slot: FileSlot::new(0),
        },
    )
    .or_die("submit");
    io.tick(true, out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 0);
}

fn check_submit_all_partial_refusal(dir: &Path) {
    let limits = Limits::BASE
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_files_max(2)
        .with_ops_max(4);
    let mut io = UringIo::new(limits).or_die("UringIo::new");
    let mut out = Completions::new(8);
    let bad = format!("{}/no_such_dir_xyz/bad.bin\0", dir.display());
    let good = format!("{}/after_bad.bin\0", dir.display());
    io.buf(BufId::new(0))[..bad.len()].copy_from_slice(bad.as_bytes());
    io.buf(BufId::new(1))[..good.len()].copy_from_slice(good.as_bytes());

    // Submit a kernel-refused open ahead of a valid open in the same batch.
    io.submit(
        Token::new(10),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: bad.len() as u32,
            flags: OpenFlags::NONE.with_read(true),
        },
    )
    .or_die("submit");
    io.submit(
        Token::new(11),
        Op::FileOpen {
            slot: FileSlot::new(1),
            path_buf: BufId::new(1),
            path_off: 0,
            path_len: good.len() as u32,
            flags: OpenFlags::NONE
                .with_read(true)
                .with_write(true)
                .with_create(true),
        },
    )
    .or_die("submit");
    let mut r10 = None;
    let mut r11 = None;
    for _ in 0..1000 {
        io.tick(true, &mut out).or_die("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == 10 {
                r10 = Some(c.result());
            } else if c.token().get() == 11 {
                r11 = Some(c.result());
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
    io.submit(
        Token::new(12),
        Op::FileClose {
            slot: FileSlot::new(1),
        },
    )
    .or_die("submit");
    io.tick(true, &mut out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 0);
}

fn check_over_16384_buffers(dir: &Path) {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(4096)
        .with_small_buffers(16_400)
        .with_small_buffer_size(256)
        .with_files_max(2)
        .with_ops_max(4);
    let mut io = UringIo::new(limits).or_die("UringIo::new with 16401 buffers");
    let mut out = Completions::new(8);
    let path = format!("{}/many_bufs.bin\0", dir.display());
    io.buf(BufId::new(0))[..path.len()].copy_from_slice(path.as_bytes());
    io.submit(
        Token::new(1),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(0),
            path_off: 0,
            path_len: path.len() as u32,
            flags: OpenFlags::NONE
                .with_read(true)
                .with_write(true)
                .with_create(true),
        },
    )
    .or_die("submit");
    io.tick(true, &mut out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 0);

    let last = limits.small(16_399);
    io.buf(last)[..64].fill(0x5E);
    io.submit(
        Token::new(2),
        Op::FileWrite {
            slot: FileSlot::new(0),
            buf: last,
            off: 0,
            len: 64,
            file_off: 0,
        },
    )
    .or_die("submit");
    io.tick(true, &mut out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 64);

    io.submit(
        Token::new(3),
        Op::FileRead {
            slot: FileSlot::new(0),
            buf: BufId::new(0),
            off: 0,
            len: 64,
            file_off: 0,
        },
    )
    .or_die("submit");
    io.tick(true, &mut out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 64);
    assert!(io.buf(BufId::new(0))[..64].iter().all(|&b| b == 0x5E));

    io.submit(
        Token::new(4),
        Op::FileClose {
            slot: FileSlot::new(0),
        },
    )
    .or_die("submit");
    io.tick(true, &mut out).or_die("tick");
    assert_eq!(out.pop().or_die("a completion").result(), 0);
}
