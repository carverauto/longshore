// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The simulated disk of the durable-log check: one that can lose what
//! was not synced, the log file on it, and reading it on the bare backend
//! once the power is on again.

use longshore_common::io::extents::Extent;
use longshore_common::io::{Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore_common::sim::{DiskConfig, Faults, SimIo};
use longshore_tcp::check::STORAGE;

pub(super) const FILE: FileSlot = FileSlot::new(0);
/// The directory the log is in: a new file is there after a power loss
/// only when its directory was synced.
pub(super) const DIR: FileSlot = FileSlot::new(1);
pub(super) const BLOCK: u32 = 4096;
pub(super) const OPEN: u64 = STORAGE;
pub(super) const WRITE: u64 = STORAGE + 1;
pub(super) const SYNC: u64 = STORAGE + 2;
pub(super) const READ: u64 = STORAGE + 3;
pub(super) const DIR_OPEN: u64 = STORAGE + 4;
pub(super) const DIR_SYNC: u64 = STORAGE + 5;
pub(super) const PATH: &str = "/durable.dat";

/// A simulator whose disk keeps writes that are not synced apart from
/// what is durable, so that a power loss can lose or tear them.
pub(super) fn sim(limits: Limits, seed: u64, faults: Faults) -> SimIo {
    let mut io = SimIo::new(limits, seed, faults);
    io.setup_disk(
        DiskConfig::BASE
            .with_max_files(4)
            .with_max_dirs(4)
            .with_max_bytes_per_file(1 << 20)
            .with_max_name_len(256)
            .with_write_log_entries(32)
            .with_write_log_bytes(64 * 1024),
    );
    io
}

/// Open the log, creating and emptying it when `create` is set.
pub(super) fn open(io: &mut SimIo, scratch: Extent, create: bool) {
    let bytes = io.buf(scratch.buf());
    bytes[..PATH.len()].copy_from_slice(PATH.as_bytes());
    bytes[PATH.len()] = 0;
    let flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(create)
        .with_empty(create)
        .with_direct(true);
    let op = Op::FileOpen {
        slot: FILE,
        path_buf: scratch.buf(),
        path_off: 0,
        path_len: PATH.len() as u32 + 1,
        flags,
    };
    io.submit(Token::new(OPEN), op).unwrap();
}

/// The block of `round` in the log after the power is on again.
pub(super) fn read_back(io: &mut SimIo, back: Extent, round: u32) -> Vec<u8> {
    let read = Op::FileRead {
        slot: FILE,
        buf: back.buf(),
        off: 0,
        len: BLOCK,
        file_off: u64::from(round) * u64::from(BLOCK),
    };
    io.submit(Token::new(READ), read).unwrap();
    let got = wait(io, READ);
    let got = usize::try_from(got).unwrap_or(0);
    io.buf(back.buf())[..got].to_vec()
}

/// Tick the bare backend until `token` completes: its result.
pub(super) fn wait(io: &mut SimIo, token: u64) -> i32 {
    let mut out = Completions::new(8);
    for _ in 0..1000 {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == token {
                return c.result();
            }
        }
    }
    panic!("token {token} never completed");
}
