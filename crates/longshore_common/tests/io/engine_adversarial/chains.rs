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

    let mut sim = SimIo::new(
        Limits {
            buffers: 1,
            buffer_size: 64,
            files_max: 1,
            ops_max: 2,
            ..Limits::BASE
        },
        1,
        Faults::default(),
    );
    let mut out = Completions::new(1);
    sim.submit(Token(1), Op::FileClose { slot: FileSlot(0) })
        .unwrap();
    sim.submit(Token(2), Op::FileClose { slot: FileSlot(0) })
        .unwrap();
    assert_eq!(sim.submit(Token(3), Op::Timeout { nanos: 0 }), Err(Busy));

    sim.tick(false, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, -EBADF);
    sim.submit(Token(3), Op::Timeout { nanos: 0 }).unwrap();
    assert_eq!(sim.submit(Token(4), Op::Timeout { nanos: 0 }), Err(Busy));
    sim.tick(false, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().token, Token(2));
    sim.tick(false, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().token, Token(3));

    // Failing 2-op chain populates ready queue with 2 items -> Busy until drained.
    let p = b"/x\0";
    sim.buf(BufId(0))[..p.len()].copy_from_slice(p);
    let open = Op::FileOpen {
        slot: FileSlot(99),
        path_buf: BufId(0),
        path_off: 0,
        path_len: p.len() as u32,
        flags: OpenFlags {
            read: true,
            ..Default::default()
        },
    };
    let close = Op::FileClose { slot: FileSlot(99) };
    sim.submit_chain(&[(Token(10), open), (Token(11), close)])
        .unwrap();
    assert_eq!(sim.submit(Token(12), Op::Timeout { nanos: 0 }), Err(Busy));

    sim.tick(false, &mut out).unwrap();
    assert_eq!(
        out.pop().unwrap(),
        longshore_common::io::Completion::new(Token(10), -EBADF)
    );
    assert_eq!(
        sim.submit_chain(&[(Token(13), open), (Token(14), close)]),
        Err(Busy)
    );
    sim.tick(false, &mut out).unwrap();
    assert_eq!(
        out.pop().unwrap(),
        longshore_common::io::Completion::new(Token(11), -ECANCELED)
    );
}

#[cfg(feature = "sim")]
#[test]
fn file_table_multi_reopen_chain_failure_recovers_direct_and_cycles_sim_gen() {
    use longshore_common::io::{
        BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ECANCELED, ENOENT,
    };
    use longshore_common::sim::{DiskConfig, Faults, SimIo};

    let mut sim = SimIo::new(
        Limits {
            buffers: 3,
            buffer_size: 4096,
            aligned_buffers: 1,
            aligned_buffer_size: 4096,
            block_size: 4096,
            files_max: 2,
            ops_max: 8,
            ..Limits::BASE
        },
        7,
        Faults::default(),
    );
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 2,
        max_bytes_per_file: 4096,
        max_name_len: 64,
        write_log_entries: 8,
        write_log_bytes: 4096,
        read_error_sectors: 4,
        sector_size: 512,
    });
    let mut out = Completions::new(16);

    let p_good = b"/good.dat\0";
    let p_miss = b"/no_dir/missing.dat\0";
    sim.buf(BufId(0))[..p_good.len()].copy_from_slice(p_good);
    sim.buf(BufId(1))[..p_miss.len()].copy_from_slice(p_miss);

    let f_create = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let f_direct_miss = OpenFlags {
        read: true,
        direct: true,
        ..Default::default()
    };

    let chain = [
        (
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: p_good.len() as u32,
                flags: f_create,
            },
        ),
        (Token(2), Op::FileClose { slot: FileSlot(0) }),
        (
            Token(3),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(1),
                path_off: 0,
                path_len: p_miss.len() as u32,
                flags: f_direct_miss,
            },
        ),
        (Token(4), Op::FileClose { slot: FileSlot(0) }),
        (
            Token(5),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: p_good.len() as u32,
                flags: f_create,
            },
        ),
    ];
    sim.submit_chain(&chain).unwrap();

    let mut res = Vec::new();
    while res.len() < 5 {
        sim.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            res.push((c.token.0, c.result));
        }
    }
    assert_eq!(
        res,
        vec![
            (1, 0),
            (2, 0),
            (3, -ENOENT),
            (4, -ECANCELED),
            (5, -ECANCELED),
        ]
    );

    let reopen = [
        (
            Token(10),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: BufId(0),
                path_off: 0,
                path_len: p_good.len() as u32,
                flags: f_create,
            },
        ),
        (
            Token(11),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 7,
                file_off: 0,
            },
        ),
        (Token(12), Op::FileClose { slot: FileSlot(0) }),
    ];
    sim.submit_chain(&reopen).unwrap();
    let mut got = Vec::new();
    while got.len() < 3 {
        sim.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            got.push((c.token.0, c.result));
        }
    }
    assert_eq!(got, vec![(10, 0), (11, 7), (12, 0)]);

    for round in 0..64u64 {
        let tok = 100 + round * 2;
        sim.submit_chain(&[
            (
                Token(tok),
                Op::FileOpen {
                    slot: FileSlot(0),
                    path_buf: BufId(0),
                    path_off: 0,
                    path_len: p_good.len() as u32,
                    flags: f_create,
                },
            ),
            (Token(tok + 1), Op::FileClose { slot: FileSlot(0) }),
        ])
        .unwrap();
        let mut n = 0;
        while n < 2 {
            sim.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert_eq!(c.result, 0);
                n += 1;
            }
        }
    }
}
