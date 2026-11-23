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

/// Opens `/sealed.dat` (path in buffer 0, `path_len` long) for direct I/O,
/// writes aligned buffer `a` to it and syncs it, all as one batch.
#[cfg(test)]
fn open_write_sync(sim: &mut SimIo, out: &mut Completions, path_len: u32, a: BufId) {
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        direct: true,
        ..Default::default()
    };
    sim.submit(Token(1), open(path_len, rw)).unwrap();
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
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            assert!(c.result >= 0);
            done += 1;
        }
    }
}

/// Submits `op` as `tok`, ticks once; the result of the completion it brings.
#[cfg(test)]
fn one_tick(sim: &mut SimIo, out: &mut Completions, tok: u64, op: Op) -> i32 {
    sim.submit(Token(tok), op).unwrap();
    sim.tick(true, out).unwrap();
    out.pop().unwrap().result
}

/// Renames the path in buffer 0 to the one in buffer 1.
fn rename(old_len: u32, new_len: u32) -> Op {
    Op::FileRename {
        old_buf: BufId(0),
        old_off: 0,
        old_len,
        new_buf: BufId(1),
        new_off: 0,
        new_len,
    }
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
    let p = put_path(&mut sim, BufId(0), b"/sealed.dat\0");
    let n = put_path(&mut sim, BufId(1), b"/sealed2.dat\0");
    let a = limits.aligned(0);
    sim.buf(a).fill(0x3C);

    sealed(|| {
        open_write_sync(&mut sim, &mut out, p, a);
        let close = Op::FileClose { slot: FileSlot(0) };
        assert_eq!(one_tick(&mut sim, &mut out, 4, close), 0);
        assert_eq!(one_tick(&mut sim, &mut out, 5, rename(p, n)), 0);
    });
}

/// Submits `op` as `tok` and ticks until it completes; its result.
#[cfg(test)]
fn drive(io: &mut SimIo, ready: &mut Completions, tok: u64, op: Op) -> i32 {
    io.submit(Token(tok), op).unwrap();
    loop {
        io.tick(true, ready).unwrap();
        if let Some(c) = ready.pop() {
            assert_eq!(c.token, Token(tok));
            return c.result;
        }
    }
}

/// Writes the NUL-terminated `s` into `buf`; its length.
fn put_path(io: &mut SimIo, buf: BufId, s: &[u8]) -> u32 {
    io.buf(buf)[..s.len()].copy_from_slice(s);
    s.len() as u32
}

/// Writes `path` into buffer 0 and drives `op(path length)` as `tok`.
#[cfg(test)]
fn at_path(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    path: &[u8],
    op: impl FnOnce(u32) -> Op,
) -> i32 {
    let len = put_path(sim, BufId(0), path);
    drive(sim, out, tok, op(len))
}

fn mkdir(path_len: u32) -> Op {
    Op::FileMkdir {
        path_buf: BufId(0),
        path_off: 0,
        path_len,
    }
}

/// The status of the path in buffer 0, into buffer 1.
fn status(path_len: u32) -> Op {
    Op::FileStatus {
        path_buf: BufId(0),
        path_off: 0,
        path_len,
        out_buf: BufId(1),
        out_off: 0,
    }
}

/// Opens the path in buffer 0 into slot 0.
fn open(path_len: u32, flags: OpenFlags) -> Op {
    Op::FileOpen {
        slot: FileSlot(0),
        path_buf: BufId(0),
        path_off: 0,
        path_len,
        flags,
    }
}

/// Four bytes between slot 0 at offset 0 and `buf`.
fn write4(buf: u32) -> Op {
    Op::FileWrite {
        slot: FileSlot(0),
        buf: BufId(buf),
        off: 0,
        len: 4,
        file_off: 0,
    }
}

fn read4(buf: u32) -> Op {
    Op::FileRead {
        slot: FileSlot(0),
        buf: BufId(buf),
        off: 0,
        len: 4,
        file_off: 0,
    }
}

/// Renaming `/d1` over `/longdir` would make `/d1/123456789` too long: the
/// rename fails with `ENAMETOOLONG` and leaves both trees in place.
#[cfg(test)]
fn rename_that_would_overflow_a_name_changes_nothing(sim: &mut SimIo, out: &mut Completions) {
    use longshore_common::io::posix::errno::linux::ENAMETOOLONG;

    assert_eq!(at_path(sim, out, 1, b"/d1\0", mkdir), 0);
    assert_eq!(at_path(sim, out, 2, b"/d1/123456789\0", mkdir), 0);
    assert_eq!(at_path(sim, out, 3, b"/longdir\0", mkdir), 0);

    let old_len = put_path(sim, BufId(0), b"/d1\0");
    let new_len = put_path(sim, BufId(1), b"/longdir\0");
    assert_eq!(drive(sim, out, 4, rename(old_len, new_len)), -ENAMETOOLONG);

    assert_eq!(at_path(sim, out, 5, b"/longdir\0", status), 0);
    assert_eq!(at_path(sim, out, 6, b"/d1/123456789\0", status), 0);
}

/// (read: false, write: false) behaves as O_RDONLY, matching POSIX/Linux.
#[cfg(test)]
fn default_flags_open_read_only(sim: &mut SimIo, out: &mut Completions) {
    use longshore_common::io::{file_size, EBADF};

    let f = put_path(sim, BufId(0), b"/ro.dat\0");
    let rw = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    assert_eq!(drive(sim, out, 7, open(f, rw)), 0);
    sim.buf(BufId(1))[..4].copy_from_slice(b"data");
    assert_eq!(drive(sim, out, 8, write4(1)), 4);
    assert_eq!(drive(sim, out, 9, Op::FileClose { slot: FileSlot(0) }), 0);
    assert_eq!(drive(sim, out, 10, open(f, OpenFlags::default())), 0);
    assert_eq!(drive(sim, out, 11, read4(2)), 4);
    assert_eq!(&sim.buf(BufId(2))[..4], b"data");
    assert_eq!(drive(sim, out, 12, write4(1)), -EBADF);
    assert_eq!(drive(sim, out, 13, Op::FileClose { slot: FileSlot(0) }), 0);
    assert_eq!(drive(sim, out, 14, status(f)), 0);
    assert_eq!(file_size(sim.buf(BufId(1))), 4);
}

#[test]
fn sim_rename_enametoolong_is_atomic_and_default_flags_read() {
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
    rename_that_would_overflow_a_name_changes_nothing(&mut sim, &mut out);
    default_flags_open_read_only(&mut sim, &mut out);
}
