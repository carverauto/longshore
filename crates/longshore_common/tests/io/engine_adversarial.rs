// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Adversarial Tier 5 tests for `Engine<W>`, `Limits::validate`, and `FileTable::check_chain`.

#[test]
fn limits_validate_allows_zero_block_size_and_zero_buffer_size() {
    use longshore_common::io::{BufId, Limits};

    let l1 = Limits {
        aligned_buffers: 0,
        block_size: 0,
        buffers: 1,
        buffer_size: 0,
        ..Limits::BASE
    };
    l1.validate();
    assert_eq!(l1.span(BufId(0)), (0, 0));
}

#[test]
#[should_panic(expected = "block_size must be 0 or a power of two >= 4096")]
fn limits_validate_rejects_nonzero_sub4k_block_size_when_aligned_zero() {
    use longshore_common::io::Limits;

    let l = Limits {
        aligned_buffers: 0,
        block_size: 2048,
        ..Limits::BASE
    };
    l.validate();
}

#[test]
#[should_panic(expected = "total buffer count overflows u32")]
fn limits_validate_rejects_u32_buffer_count_overflow() {
    use longshore_common::io::Limits;

    let l = Limits {
        buffers: u32::MAX,
        small_buffers: 1,
        ..Limits::BASE
    };
    l.validate();
}

#[cfg(feature = "sim")]
#[test]
fn timeout_u64_max_after_ticks_does_not_overflow_or_fire_early() {
    use longshore_common::io::{Completions, Io, Limits, Op, Token};
    use longshore_common::sim::{Faults, SimIo};

    let mut sim = SimIo::new(
        Limits {
            sockets_max: 1,
            ops_max: 4,
            ..Limits::BASE
        },
        1,
        Faults::default(),
    );
    let mut out = Completions::new(4);
    sim.submit(Token(1), Op::Timeout { nanos: 0 }).unwrap();
    sim.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().token, Token(1));

    // Advance clock > 0, then submit u64::MAX and u64::MAX - 1.
    sim.submit(Token(10), Op::Timeout { nanos: 50_000 })
        .unwrap();
    sim.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().token, Token(10));
    assert!(sim.now_ns() > 0);

    sim.submit(Token(2), Op::Timeout { nanos: u64::MAX })
        .unwrap();
    sim.submit(
        Token(3),
        Op::Timeout {
            nanos: u64::MAX - 1,
        },
    )
    .unwrap();
    sim.submit(Token(4), Op::Timeout { nanos: 100_000 })
        .unwrap();
    sim.tick(true, &mut out).unwrap();
    let c = out.pop().unwrap();
    assert_eq!(
        c.token,
        Token(4),
        "saturating deadline must not fire before 100us timer"
    );
    sim.tick(false, &mut out).unwrap();
    assert!(out.is_empty());
}

#[cfg(feature = "sim")]
#[cfg(test)]
mod deferred_cancel {
    use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
    use longshore_common::sim::{DiskConfig, Faults, SimIo};

    pub const PATH: &[u8] = b"/defer_cancel.dat\0";

    /// A delayed simulator with a small disk and `PATH` written to buffer 0.
    pub fn delayed_sim(seed: u64) -> SimIo {
        let mut sim = SimIo::new(
            Limits {
                buffers: 2,
                buffer_size: 256,
                files_max: 2,
                ops_max: 8,
                ..Limits::BASE
            },
            seed,
            Faults {
                delay: 500_000,
                ..Default::default()
            },
        );
        sim.setup_disk(DiskConfig {
            max_files: 4,
            max_dirs: 2,
            max_bytes_per_file: 4096,
            max_name_len: 64,
            ..DiskConfig::BASE
        });
        sim.buf(BufId(0))[..PATH.len()].copy_from_slice(PATH);
        sim
    }

    /// Opens `PATH` (read, write, create) into slot 0.
    pub fn open() -> Op {
        Op::FileOpen {
            slot: FileSlot(0),
            path_buf: BufId(0),
            path_off: 0,
            path_len: PATH.len() as u32,
            flags: OpenFlags {
                read: true,
                write: true,
                create: true,
                ..Default::default()
            },
        }
    }

    /// A 16-byte write or read on slot 0, the op that waits behind the open.
    pub fn deferred(use_write: bool) -> Op {
        if use_write {
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 16,
                file_off: 0,
            }
        } else {
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 16,
                file_off: 0,
            }
        }
    }

    /// Ticks until tokens 1, 2 and 3 have all completed; their results.
    pub fn results_of_three(sim: &mut SimIo, out: &mut Completions) -> [i32; 3] {
        let mut got = [None; 3];
        while got.iter().any(Option::is_none) {
            sim.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                got[(c.token.0 - 1) as usize] = Some(c.result);
            }
        }
        got.map(Option::unwrap)
    }

    /// Submits `op` under `token`.
    pub fn submit(sim: &mut SimIo, token: u64, op: Op) {
        sim.submit(Token(token), op).unwrap();
    }
}

#[cfg(feature = "sim")]
#[test]
fn cancelled_file_open_releases_deferred_read_and_write_with_ebadf_and_reuses_slot() {
    use deferred_cancel::{deferred, delayed_sim, open, results_of_three, submit};
    use longshore_common::io::{Completions, Io, Op, Token, EBADF, ECANCELED};

    for use_write in [false, true] {
        let mut saw_cancel = false;
        for seed in 0..32u64 {
            let mut sim = delayed_sim(seed);
            let mut out = Completions::new(8);
            submit(&mut sim, 1, open());
            submit(&mut sim, 2, deferred(use_write));
            submit(&mut sim, 3, Op::Cancel { target: Token(1) });

            let [r1, r2, r3] = results_of_three(&mut sim, &mut out);
            if r1 == -ECANCELED {
                saw_cancel = true;
                assert_eq!(r3, 0);
                assert_eq!(r2, -EBADF);
                submit(&mut sim, 4, open());
                sim.tick(true, &mut out).unwrap();
                assert_eq!(out.pop().unwrap().result, 0);
                break;
            }
        }
        assert!(saw_cancel, "use_write={use_write} never observed cancel");
    }
}

#[cfg(unix)]
#[test]
fn posix_close_cancels_attached_connect_and_frees_slot() {
    use longshore_common::io::posix::PosixIo;
    use longshore_common::io::{Completions, Io, Limits, Op, Sock, Token, ECANCELED};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};

    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let mut io = PosixIo::new(Limits {
        sockets_max: 2,
        ops_max: 4,
        ..Limits::BASE
    })
    .unwrap();
    let mut out = Completions::new(4);

    // Try loopback connect + immediate close; also test non-routable connect so
    // Connect is attached (SockKind::Connecting) before Close arrives.
    let blackhole: SocketAddr = "192.0.2.1:80".parse().unwrap();
    io.submit(
        Token(1),
        Op::Connect {
            sock: Sock(0),
            addr: blackhole,
        },
    )
    .unwrap();
    io.tick(false, &mut out).unwrap();
    let mut got_connect_early = false;
    while let Some(c) = out.pop() {
        if c.token == Token(1) {
            got_connect_early = true;
        }
    }
    io.submit(Token(2), Op::Close { sock: Sock(0) }).unwrap();
    let mut got_close = false;
    while !got_close || !got_connect_early {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == Token(1) {
                assert!(c.result == -ECANCELED || c.result < 0);
                got_connect_early = true;
            } else if c.token == Token(2) {
                assert_eq!(c.result, 0);
                got_close = true;
            }
        }
    }

    // Slot 0 is reusable for a fresh Connect.
    io.submit(
        Token(3),
        Op::Connect {
            sock: Sock(0),
            addr,
        },
    )
    .unwrap();
    io.tick(true, &mut out).unwrap();
    assert_eq!(out.pop().unwrap().result, 0);
    drop(listener);
}

#[path = "engine_adversarial/chains.rs"]
mod chains;
