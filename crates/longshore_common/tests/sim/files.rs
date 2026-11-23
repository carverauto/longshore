// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared file-operations contract and zero-allocation tests on [`SimIo`].

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::contract_files;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[global_allocator]
static ALLOC: Guard = Guard;

fn make_sim(limits: Limits, delay: u32) -> SimIo {
    let mut sim = SimIo::new(
        limits,
        77,
        Faults {
            delay,
            ..Default::default()
        },
    );
    sim.setup_disk(DiskConfig {
        max_files: 32,
        max_dirs: 16,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 256,
        ..DiskConfig::BASE
    });
    sim
}

#[test]
fn sim_file_contract_and_zero_alloc() {
    contract_files::run_all(&mut |l| make_sim(l, 0), "");
    contract_files::run_all(&mut |l| make_sim(l, 200_000), "");

    let limits = Limits {
        buffers: 2,
        buffer_size: 4096,
        files_max: 2,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut sim = make_sim(limits, 0);
    let mut out = Completions::new(8);
    let p = b"/sealed.dat\0";
    let n = b"/sealed2.dat\0";
    sim.buf(BufId(0))[..p.len()].copy_from_slice(p);
    sim.buf(BufId(1))[..n.len()].copy_from_slice(n);
    let a = limits.aligned(0);
    sim.buf(a).fill(0x3C);

    sealed(|| {
        let rw = OpenFlags {
            read: true,
            write: true,
            create: true,
            direct: true,
            ..Default::default()
        };
        sim.submit(
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: p.len() as u32,
                flags: rw,
            },
        )
        .unwrap();
        sim.submit(
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: a,
                off: 0,
                len: 4096,
                file_off: 0,
            },
        )
        .unwrap();
        sim.submit(
            Token(3),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        )
        .unwrap();
        let mut done = 0;
        while done < 3 {
            sim.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert!(c.result >= 0);
                done += 1;
            }
        }
        sim.submit(Token(4), Op::FileClose { slot: FileSlot(0) })
            .unwrap();
        sim.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, 0);
        sim.submit(
            Token(5),
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len: p.len() as u32,
                new_buf: BufId(1),
                new_off: 0,
                new_len: n.len() as u32,
            },
        )
        .unwrap();
        sim.tick(true, &mut out).unwrap();
        assert_eq!(out.pop().unwrap().result, 0);
    });
}

#[test]
fn sim_rename_enametoolong_is_atomic_and_default_flags_read() {
    use longshore_common::io::posix::errno::linux::ENAMETOOLONG;
    use longshore_common::io::{file_size, EBADF};

    let limits = Limits {
        buffers: 4,
        buffer_size: 512,
        files_max: 4,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut sim = SimIo::new(limits, 99, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 16,
        max_dirs: 16,
        max_bytes_per_file: 4096,
        max_name_len: 16,
        ..DiskConfig::BASE
    });
    let mut out = Completions::new(8);
    let drive = |io: &mut SimIo, ready: &mut Completions, tok: u64, op: Op| -> i32 {
        io.submit(Token(tok), op).unwrap();
        loop {
            io.tick(true, ready).unwrap();
            if let Some(c) = ready.pop() {
                assert_eq!(c.token, Token(tok));
                return c.result;
            }
        }
    };
    let put_path = |io: &mut SimIo, buf: BufId, s: &[u8]| -> u32 {
        io.buf(buf)[..s.len()].copy_from_slice(s);
        s.len() as u32
    };

    let d1 = put_path(&mut sim, BufId(0), b"/d1\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            1,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: d1,
            },
        ),
        0
    );
    let child = put_path(&mut sim, BufId(0), b"/d1/123456789\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            2,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: child,
            },
        ),
        0
    );
    let longdir = put_path(&mut sim, BufId(0), b"/longdir\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            3,
            Op::FileMkdir {
                path_buf: BufId(0),
                path_off: 0,
                path_len: longdir,
            },
        ),
        0
    );

    let old_len = put_path(&mut sim, BufId(0), b"/d1\0");
    let new_len = put_path(&mut sim, BufId(1), b"/longdir\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            4,
            Op::FileRename {
                old_buf: BufId(0),
                old_off: 0,
                old_len,
                new_buf: BufId(1),
                new_off: 0,
                new_len,
            },
        ),
        -ENAMETOOLONG
    );

    let ld = put_path(&mut sim, BufId(0), b"/longdir\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            5,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ld,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        0
    );
    let ch = put_path(&mut sim, BufId(0), b"/d1/123456789\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            6,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: ch,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        0
    );

    // (read: false, write: false) behaves as O_RDONLY, matching POSIX/Linux.
    let f = put_path(&mut sim, BufId(0), b"/ro.dat\0");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            7,
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: f,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    ..Default::default()
                },
            },
        ),
        0
    );
    sim.buf(BufId(1))[..4].copy_from_slice(b"data");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            8,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4,
                file_off: 0,
            },
        ),
        4
    );
    assert_eq!(
        drive(&mut sim, &mut out, 9, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            10,
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: f,
                flags: OpenFlags::default(),
            },
        ),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            11,
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 4,
                file_off: 0,
            },
        ),
        4
    );
    assert_eq!(&sim.buf(BufId(2))[..4], b"data");
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            12,
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 4,
                file_off: 0,
            },
        ),
        -EBADF
    );
    assert_eq!(
        drive(&mut sim, &mut out, 13, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            14,
            Op::FileStatus {
                path_buf: BufId(0),
                path_off: 0,
                path_len: f,
                out_buf: BufId(1),
                out_off: 0,
            },
        ),
        0
    );
    assert_eq!(file_size(sim.buf(BufId(1))), 4);
}
