// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 & Tier 2 E2E tests for Feature 1: `Limits` base value & migration (`#41`).

#[cfg(unix)]
use crate::e2e::harness::{drive_until, posix_backend};
use longshore::io::{BufId, Limits};
#[cfg(unix)]
use longshore::io::{Completions, Io, Op, Sock, Token};
#[cfg(feature = "sim")]
use longshore::sim::{scenario::echo, Faults, SimIo};
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    crate::e2e::harness::workspace_root()
}

#[test]
fn t1_f01_1_limits_base_and_default_fields() {
    let base = Limits::BASE;
    let def = Limits::default();
    assert_eq!(def.sockets_max(), base.sockets_max());
    assert_eq!(def.buffers(), base.buffers());
    assert_eq!(def.buffer_size(), base.buffer_size());
    assert_eq!(def.small_buffers(), base.small_buffers());
    assert_eq!(def.small_buffer_size(), base.small_buffer_size());
    assert_eq!(def.ops_max(), base.ops_max());
    assert_eq!(base.buffer_count(), 0);
    assert_eq!(base.pool_bytes(), 0);

    let custom = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_small_buffers(3)
        .with_small_buffer_size(512)
        .with_ops_max(8);
    assert_eq!(custom.sockets_max(), 4);
    assert_eq!(custom.buffer_count(), 5);
    assert_eq!(
        custom.pool_bytes(),
        2 * 4096 + 3 * 512 + custom.block_size() as usize
    );
}

#[cfg(feature = "sim")]
#[test]
fn t1_f01_2_simio_with_limits_base_runs_echo() {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(3)
        .with_buffer_size(4096)
        .with_ops_max(4);
    let mut io = SimIo::new(limits, 1, Faults::default());
    let mut out = longshore::io::Completions::new(8);
    let addr = "192.0.2.10:443".parse().expect("addr");
    let stats = echo(&mut io, addr, 16 * 1024, 4096, false, &mut out).expect("sim echo");
    assert!(stats.sends() > 0 && stats.recvs() > 0 && stats.ticks() > 0);
}

#[cfg(unix)]
#[test]
fn t1_f01_3_posix_engine_with_limits_base_connects() {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(3)
        .with_buffer_size(4096)
        .with_ops_max(4);
    let mut io = posix_backend(limits).expect("posix backend");
    let mut out = Completions::new(8);
    let addr = "127.0.0.1:0".parse().expect("addr");
    let bound = io.listen(Sock::new(0), addr).expect("listen");
    io.submit(
        Token::new(1),
        Op::Accept {
            listener: Sock::new(0),
            sock: Sock::new(1),
        },
    )
    .expect("accept");
    io.submit(
        Token::new(2),
        Op::Connect {
            sock: Sock::new(2),
            addr: bound,
        },
    )
    .expect("connect");
    let cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 2).expect("connect pair");
    assert!(cs.iter().all(|c| c.result() == 0), "unexpected: {cs:?}");
    for (idx, sock) in [Sock::new(2), Sock::new(1), Sock::new(0)]
        .into_iter()
        .enumerate()
    {
        io.submit(Token::new(10 + idx as u64), Op::Close { sock })
            .expect("close");
    }
    let closed = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 3).expect("close");
    assert_eq!(closed.len(), 3);
}

#[test]
fn t1_f01_4_small_buffers_and_span_tiling() {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(2)
        .with_buffer_size(1024)
        .with_small_buffers(4)
        .with_small_buffer_size(256)
        .with_ops_max(4);
    assert_eq!(limits.buffer_count(), 6);
    assert_eq!(limits.small(0), BufId::new(2));
    assert_eq!(limits.small(3), BufId::new(5));

    let mut cursor = 0usize;
    for id in 0..limits.buffer_count() {
        let (off, len) = limits.span(BufId::new(id));
        assert_eq!(off, cursor, "buffer {id} must abut previous");
        let expected_len = if id < limits.buffers() { 1024 } else { 256 };
        assert_eq!(len, expected_len);
        assert_eq!(limits.range(BufId::new(id), 10, 20), (off + 10)..(off + 30));
        cursor += len;
    }
    assert_eq!(cursor + limits.block_size() as usize, limits.pool_bytes());
}

/// Every `Limits` starts from `Limits::BASE`. Its fields are private, so
/// `Limits::BASE` (or `Limits::default()`, the same value) and the `with_*`
/// setters are the only way to build one; the compiler now enforces what a
/// scan for `..Limits::BASE` in struct literals once checked. What is left to
/// check is that the whole workspace builds.
#[test]
fn t1_f01_5_workspace_limits_migration_and_cargo_check() {
    let root = workspace_root();
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let out = Command::new(cargo)
        .args(["check", "--workspace", "--all-targets"])
        .current_dir(&root)
        .output()
        .expect("run cargo check");
    assert!(
        out.status.success(),
        "cargo check failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn t2_f01_1_zero_buffers_limits_pool_bytes() {
    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(0)
        .with_buffer_size(4096)
        .with_small_buffers(0)
        .with_small_buffer_size(512)
        .with_ops_max(2);
    assert_eq!(limits.buffer_count(), 0);
    assert_eq!(limits.pool_bytes(), 0);
    assert_eq!(Limits::BASE.buffer_count(), 0);
    assert_eq!(Limits::BASE.pool_bytes(), 0);
}

#[test]
#[should_panic(expected = "small buffer out of range")]
fn t2_f01_2_small_out_of_range_panics() {
    let _: BufId = Limits::BASE.small(0);
}

#[test]
#[should_panic(expected = "buffer out of range")]
fn t2_f01_3_span_out_of_range_panics() {
    let _: (usize, usize) = Limits::BASE.span(BufId::new(u32::MAX));
}

#[test]
fn t2_f01_4_range_out_of_bounds_and_overflow_panics() {
    let limits = Limits::BASE.with_buffers(1).with_buffer_size(64);
    let past_end =
        std::panic::catch_unwind(|| limits.range(BufId::new(0), limits.buffer_size(), 1));
    assert!(past_end.is_err(), "1 byte past end must panic");

    let overflow = std::panic::catch_unwind(|| limits.range(BufId::new(0), u32::MAX, 1));
    let err = overflow.expect_err("u32 overflow must panic");
    let msg = err
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| err.downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("");
    assert!(msg.contains("buffer range out of bounds"), "got: {msg}");
}

#[test]
#[should_panic(expected = "buf_pair needs two distinct pool buffers")]
fn t2_f01_5_pair_same_buffer_panics() {
    let limits = Limits::BASE.with_buffers(2).with_buffer_size(64);
    let mut pool = vec![0u8; limits.pool_bytes()];
    let _: (&mut [u8], &mut [u8]) = limits.pair(&mut pool, BufId::new(0), BufId::new(0));
}
