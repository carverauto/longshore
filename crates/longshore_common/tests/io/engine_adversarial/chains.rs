// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial tests for `Engine` ready queue capacity and `FileTable::check_chain`.

#[cfg(feature = "sim")]
#[test]
fn ready_queue_refusals_count_toward_ops_max_busy_until_drained() {
    use longshore_common::io::{
        BufId, Busy, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, EBADF, ECANCELED,
    };
    use longshore_common::sim::{Faults, SimIo};
    const SLOT_0: FileSlot = FileSlot::new(0);
    const TOKEN_3: Token = Token::new(3);

    let mut sim = SimIo::new(
        Limits::BASE
            .with_buffers(1)
            .with_buffer_size(64)
            .with_files_max(1)
            .with_ops_max(2),
        1,
        Faults::default(),
    );
    let mut out = Completions::new(1);
    sim.submit(Token::new(1), Op::FileClose { slot: SLOT_0 })
        .unwrap();
    sim.submit(Token::new(2), Op::FileClose { slot: SLOT_0 })
        .unwrap();
    assert_eq!(sim.submit(TOKEN_3, Op::Timeout { nanos: 0 }), Err(Busy));

    sim.tick(false, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result(), -EBADF);
    sim.submit(TOKEN_3, Op::Timeout { nanos: 0 }).unwrap();
    assert_eq!(
        sim.submit(Token::new(4), Op::Timeout { nanos: 0 }),
        Err(Busy)
    );
    sim.tick(false, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().token(), Token::new(2));
    sim.tick(false, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().token(), TOKEN_3);

    // Failing 2-op chain populates ready queue with 2 items -> Busy until drained.
    let p = b"/x\0";
    sim.buf(BufId::new(0))[..p.len()].copy_from_slice(p);
    let open = Op::FileOpen {
        slot: FileSlot::new(99),
        path_buf: BufId::new(0),
        path_off: 0,
        path_len: p.len() as u32,
        flags: OpenFlags::NONE.with_read(true),
    };
    let close = Op::FileClose {
        slot: FileSlot::new(99),
    };
    sim.submit_chain(&[(Token::new(10), open), (Token::new(11), close)])
        .unwrap();
    assert_eq!(
        sim.submit(Token::new(12), Op::Timeout { nanos: 0 }),
        Err(Busy)
    );

    sim.tick(false, &mut out).unwrap();
    assert_eq!(
        out.pop().unwrap(),
        longshore_common::io::Completion::new(Token::new(10), -EBADF)
    );
    assert_eq!(
        sim.submit_chain(&[(Token::new(13), open), (Token::new(14), close)]),
        Err(Busy)
    );
    sim.tick(false, &mut out).unwrap();
    assert_eq!(
        out.pop().unwrap(),
        longshore_common::io::Completion::new(Token::new(11), -ECANCELED)
    );
}

#[cfg(feature = "sim")]
#[cfg(test)]
mod reopen {
    use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags};
    use longshore_common::sim::{DiskConfig, Faults, SimIo};

    pub const GOOD: &[u8] = b"/good.dat\0";
    pub const MISSING: &[u8] = b"/no_dir/missing.dat\0";

    pub fn create() -> OpenFlags {
        OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true)
    }

    pub fn direct_read() -> OpenFlags {
        OpenFlags::NONE.with_read(true).with_direct(true)
    }

    /// Seed 7, a small disk, `GOOD` in buffer 0 and `MISSING` in buffer 1.
    pub fn sim() -> SimIo {
        let mut sim = SimIo::new(
            Limits::BASE
                .with_buffers(3)
                .with_buffer_size(4096)
                .with_aligned_buffers(1)
                .with_aligned_buffer_size(4096)
                .with_block_size(4096)
                .with_files_max(2)
                .with_ops_max(8),
            7,
            Faults::default(),
        );
        sim.setup_disk(
            DiskConfig::BASE
                .with_max_files(4)
                .with_max_dirs(2)
                .with_max_bytes_per_file(4096)
                .with_max_name_len(64)
                .with_write_log_entries(8)
                .with_write_log_bytes(4096)
                .with_read_error_sectors(4)
                .with_sector_size(512),
        );
        sim.buf(BufId::new(0))[..GOOD.len()].copy_from_slice(GOOD);
        sim.buf(BufId::new(1))[..MISSING.len()].copy_from_slice(MISSING);
        sim
    }

    /// Opens the path in `path_buf` (`path` long) into slot 0.
    pub fn open(path_buf: u32, path: &[u8], flags: OpenFlags) -> Op {
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: BufId::new(path_buf),
            path_off: 0,
            path_len: path.len() as u32,
            flags,
        }
    }

    pub fn close() -> Op {
        Op::FileClose {
            slot: FileSlot::new(0),
        }
    }

    /// Ticks until at least `n` completions arrived; their tokens and results.
    pub fn collect(sim: &mut SimIo, out: &mut Completions, n: usize) -> Vec<(u64, i32)> {
        let mut res = Vec::new();
        while res.len() < n {
            sim.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                res.push((c.token().get(), c.result()));
            }
        }
        res
    }
}

#[cfg(feature = "sim")]
#[test]
fn file_table_multi_reopen_chain_failure_recovers_direct_and_cycles_sim_gen() {
    use longshore_common::io::{BufId, Completions, FileSlot, Io, Op, Token, ECANCELED, ENOENT};
    use reopen::{close, collect, create, direct_read, open, GOOD, MISSING};

    let mut sim = reopen::sim();
    let mut out = Completions::new(16);

    let chain = [
        (Token::new(1), open(0, GOOD, create())),
        (Token::new(2), close()),
        (Token::new(3), open(1, MISSING, direct_read())),
        (Token::new(4), close()),
        (Token::new(5), open(0, GOOD, create())),
    ];
    sim.submit_chain(&chain).unwrap();
    assert_eq!(
        collect(&mut sim, &mut out, 5),
        vec![
            (1, 0),
            (2, 0),
            (3, -ENOENT),
            (4, -ECANCELED),
            (5, -ECANCELED),
        ]
    );

    let write = Op::FileWrite {
        slot: FileSlot::new(0),
        buf: BufId::new(2),
        off: 0,
        len: 7,
        file_off: 0,
    };
    let reopen = [
        (Token::new(10), open(0, GOOD, create())),
        (Token::new(11), write),
        (Token::new(12), close()),
    ];
    sim.submit_chain(&reopen).unwrap();
    assert_eq!(
        collect(&mut sim, &mut out, 3),
        vec![(10, 0), (11, 7), (12, 0)]
    );

    for round in 0..64u64 {
        let tok = 100 + round * 2;
        sim.submit_chain(&[
            (Token::new(tok), open(0, GOOD, create())),
            (Token::new(tok + 1), close()),
        ])
        .unwrap();
        for (_, result) in collect(&mut sim, &mut out, 2) {
            assert_eq!(result, 0);
        }
    }
}
