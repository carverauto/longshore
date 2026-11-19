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
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rs_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

#[test]
fn t1_f01_1_limits_base_and_default_fields() {
    let base = Limits::BASE;
    let def = Limits::default();
    assert_eq!(def.sockets_max, base.sockets_max);
    assert_eq!(def.buffers, base.buffers);
    assert_eq!(def.buffer_size, base.buffer_size);
    assert_eq!(def.small_buffers, base.small_buffers);
    assert_eq!(def.small_buffer_size, base.small_buffer_size);
    assert_eq!(def.ops_max, base.ops_max);
    assert_eq!(base.buffer_count(), 0);
    assert_eq!(base.pool_bytes(), 0);

    let custom = Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 3,
        small_buffer_size: 512,
        ops_max: 8,
        ..Limits::BASE
    };
    assert_eq!(custom.sockets_max, 4);
    assert_eq!(custom.buffer_count(), 5);
    assert_eq!(
        custom.pool_bytes(),
        2 * 4096 + 3 * 512 + custom.block_size as usize
    );
}

#[cfg(feature = "sim")]
#[test]
fn t1_f01_2_simio_with_limits_base_runs_echo() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 4096,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = SimIo::new(limits, 1, Faults::default());
    let mut out = longshore::io::Completions::new(8);
    let addr = "192.0.2.10:443".parse().expect("addr");
    let stats = echo(&mut io, addr, 16 * 1024, 4096, false, &mut out).expect("sim echo");
    assert!(stats.sends > 0 && stats.recvs > 0 && stats.ticks > 0);
}

#[cfg(unix)]
#[test]
fn t1_f01_3_posix_engine_with_limits_base_connects() {
    let limits = Limits {
        sockets_max: 4,
        buffers: 3,
        buffer_size: 4096,
        ops_max: 4,
        ..Limits::BASE
    };
    let mut io = posix_backend(limits).expect("posix backend");
    let mut out = Completions::new(8);
    let addr = "127.0.0.1:0".parse().expect("addr");
    let bound = io.listen(Sock(0), addr).expect("listen");
    io.submit(
        Token(1),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(1),
        },
    )
    .expect("accept");
    io.submit(
        Token(2),
        Op::Connect {
            sock: Sock(2),
            addr: bound,
        },
    )
    .expect("connect");
    let cs = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 2).expect("connect pair");
    assert!(cs.iter().all(|c| c.result == 0), "unexpected: {cs:?}");
    for (idx, sock) in [Sock(2), Sock(1), Sock(0)].into_iter().enumerate() {
        io.submit(Token(10 + idx as u64), Op::Close { sock })
            .expect("close");
    }
    let closed = drive_until(&mut io, &mut out, 1_000, |v| v.len() == 3).expect("close");
    assert_eq!(closed.len(), 3);
}

#[test]
fn t1_f01_4_small_buffers_and_span_tiling() {
    let limits = Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 1024,
        small_buffers: 4,
        small_buffer_size: 256,
        ops_max: 4,
        ..Limits::BASE
    };
    assert_eq!(limits.buffer_count(), 6);
    assert_eq!(limits.small(0), BufId(2));
    assert_eq!(limits.small(3), BufId(5));

    let mut cursor = 0usize;
    for id in 0..limits.buffer_count() {
        let (off, len) = limits.span(BufId(id));
        assert_eq!(off, cursor, "buffer {id} must abut previous");
        let expected_len = if id < limits.buffers { 1024 } else { 256 };
        assert_eq!(len, expected_len);
        assert_eq!(limits.range(BufId(id), 10, 20), (off + 10)..(off + 30));
        cursor += len;
    }
    assert_eq!(cursor + limits.block_size as usize, limits.pool_bytes());
}

#[test]
fn t1_f01_5_workspace_limits_migration_and_cargo_check() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files);
    let mut unmigrated = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        if rel.ends_with("crates/longshore_common/src/io.rs")
            || rel.ends_with("crates/longshore_common/src/io/limits.rs")
            || rel.ends_with("crates/longshore_common/tests/io/props.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("read rs");
        let bytes = text.as_bytes();
        let mut idx = 0;
        while let Some(pos) = text[idx..].find("Limits {") {
            let abs = idx + pos;
            // Skip comments/strings heuristic: look forward to matching closing brace.
            if let Some(end) = text[abs..].find('}') {
                let block = &text[abs..abs + end + 1];
                let line_start = bytes[..abs]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |p| p + 1);
                let prefix = text[line_start..abs].trim_start();
                if !prefix.starts_with("//") && !prefix.ends_with("-> ") && !block.contains("..") {
                    unmigrated.push(format!("{}: {block}", rel.display()));
                }
            }
            idx = abs + 8;
        }
    }
    assert!(
        unmigrated.is_empty(),
        "Limits literals without ..Limits::BASE: {unmigrated:?}"
    );

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
    let limits = Limits {
        sockets_max: 2,
        buffers: 0,
        buffer_size: 4096,
        small_buffers: 0,
        small_buffer_size: 512,
        ops_max: 2,
        ..Limits::BASE
    };
    assert_eq!(limits.buffer_count(), 0);
    assert_eq!(limits.pool_bytes(), 0);
    assert_eq!(Limits::BASE.buffer_count(), 0);
    assert_eq!(Limits::BASE.pool_bytes(), 0);
}

#[test]
#[should_panic(expected = "small buffer out of range")]
fn t2_f01_2_small_out_of_range_panics() {
    let _ = Limits::BASE.small(0);
}

#[test]
#[should_panic(expected = "buffer out of range")]
fn t2_f01_3_span_out_of_range_panics() {
    let _ = Limits::BASE.span(BufId(u32::MAX));
}

#[test]
fn t2_f01_4_range_out_of_bounds_and_overflow_panics() {
    let limits = Limits {
        buffers: 1,
        buffer_size: 64,
        ..Limits::BASE
    };
    let past_end = std::panic::catch_unwind(|| limits.range(BufId(0), limits.buffer_size, 1));
    assert!(past_end.is_err(), "1 byte past end must panic");

    let overflow = std::panic::catch_unwind(|| limits.range(BufId(0), u32::MAX, 1));
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
    let limits = Limits {
        buffers: 2,
        buffer_size: 64,
        ..Limits::BASE
    };
    let mut pool = vec![0u8; limits.pool_bytes()];
    let _ = limits.pair(&mut pool, BufId(0), BufId(0));
}
