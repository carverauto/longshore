// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 9: Aligned pool boundaries, `getrlimit`, and `1 GiB` range splits.

#[cfg(unix)]
use super::super::harness::posix_backend;
use super::helpers::file_limits;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Limits};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

/// `T2-F09-1`: Non-power-of-2 or `< 4096` `block_size` with aligned buffers is rejected.
#[test]
fn t2_f09_1_invalid_block_size_rejected() {
    for bad_block in [0u32, 2048, 3000, 4097] {
        let limits = file_limits(2, 2, 4096, bad_block);
        #[cfg(feature = "sim")]
        {
            let res = std::panic::catch_unwind(|| {
                let _: SimIo = SimIo::new(limits, 1, Faults::default());
            });
            assert!(
                res.is_err(),
                "SimIo silently accepted invalid block_size={bad_block}"
            );
        }
        #[cfg(unix)]
        {
            let res = std::panic::catch_unwind(|| posix_backend(limits));
            let rejected = match res {
                Err(_) => true,
                Ok(Err(_)) => true,
                Ok(Ok(_)) => false,
            };
            assert!(
                rejected,
                "Posix silently accepted invalid block_size={bad_block}"
            );
        }
        #[cfg(target_os = "linux")]
        {
            let res = std::panic::catch_unwind(|| UringIo::new(limits));
            let rejected = match res {
                Err(_) => true,
                Ok(Err(_)) => true,
                Ok(Ok(_)) => false,
            };
            assert!(
                rejected,
                "UringIo silently accepted invalid block_size={bad_block}"
            );
        }
    }
}

/// `T2-F09-2`: Unprivileged child lowering `RLIMIT_MEMLOCK` gets descriptive error on `UringIo`.
#[cfg(target_os = "linux")]
#[test]
fn t2_f09_2_linux_rlimit_memlock_descriptive_error() {
    const CHILD_ENV: &str = "LONGSHORE_E2E_F09_MEMLOCK_CHILD";
    if std::env::var(CHILD_ENV).is_ok() {
        #[repr(C)]
        struct RLimit {
            rlim_cur: u64,
            rlim_max: u64,
        }
        extern "C" {
            fn getrlimit(resource: i32, rlim: *mut RLimit) -> i32;
            fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
            fn geteuid() -> u32;
            fn setuid(uid: u32) -> i32;
            fn setgid(gid: u32) -> i32;
        }
        const RLIMIT_MEMLOCK: i32 = 8;
        if unsafe { geteuid() } == 0 {
            assert_eq!(unsafe { setgid(65534) }, 0, "setgid nobody");
            assert_eq!(unsafe { setuid(65534) }, 0, "setuid nobody");
        }
        let mut orig = RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { getrlimit(RLIMIT_MEMLOCK, &mut orig) }, 0);
        let tiny = Limits {
            sockets_max: 2,
            ops_max: 4,
            ..Limits::BASE
        };
        let mut chosen = None;
        for shift in 17..=28 {
            let cap = 1u64 << shift;
            if cap > orig.rlim_max {
                break;
            }
            let lim = RLimit {
                rlim_cur: cap,
                rlim_max: orig.rlim_max,
            };
            if unsafe { setrlimit(RLIMIT_MEMLOCK, &lim) } != 0 {
                continue;
            }
            if let Ok(io) = UringIo::new(tiny) {
                drop(io);
                chosen = Some(cap);
                break;
            }
        }
        let cap = chosen.expect("found RLIMIT_MEMLOCK high enough for ring setup");
        let pool_target = (cap as u32).saturating_add(1 << 20);
        let limits = Limits {
            sockets_max: 2,
            buffers: 1,
            buffer_size: pool_target,
            ops_max: 4,
            ..Limits::BASE
        };
        let err = match UringIo::new(limits) {
            Ok(_) => panic!("expected RLIMIT_MEMLOCK failure for cap={cap}"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::OutOfMemory);
        let msg = err.to_string();
        assert!(
            msg.contains("RLIMIT_MEMLOCK"),
            "missing RLIMIT_MEMLOCK: {msg}"
        );
        assert!(
            msg.contains(&format!("{cap} bytes")),
            "error must name RLIMIT_MEMLOCK ({cap} bytes): {msg}"
        );
        assert!(
            msg.contains(&limits.pool_bytes().to_string()),
            "error must name pool_bytes ({}): {msg}",
            limits.pool_bytes()
        );
        return;
    }

    let out = std::process::Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--exact",
            "e2e::t12_pool_fileslots::f09_pool_t2::t2_f09_2_linux_rlimit_memlock_descriptive_error",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn child");
    assert!(
        out.status.success(),
        "child failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `T2-F09-3`: `sockets_max + files_max > RLIMIT_NOFILE` fails `UringIo::new` with both numbers.
#[cfg(target_os = "linux")]
#[test]
fn t2_f09_3_linux_rlimit_nofile_descriptive_error() {
    #[repr(C)]
    struct RLimit {
        rlim_cur: u64,
        rlim_max: u64,
    }
    extern "C" {
        fn getrlimit(resource: i32, rlim: *mut RLimit) -> i32;
    }
    const RLIMIT_NOFILE: i32 = 7;
    let mut rlim = RLimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(unsafe { getrlimit(RLIMIT_NOFILE, &mut rlim) }, 0);
    let total_wanted = rlim.rlim_cur.saturating_add(16).min(u32::MAX as u64) as u32;
    let sockets_max = total_wanted / 2;
    let files_max = total_wanted - sockets_max;
    let limits = Limits {
        sockets_max,
        files_max,
        buffers: 1,
        buffer_size: 4096,
        ops_max: 2,
        ..Limits::BASE
    };
    let err = match UringIo::new(limits) {
        Ok(_) => panic!("expected RLIMIT_NOFILE error"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains(&total_wanted.to_string()) && msg.contains(&rlim.rlim_cur.to_string()),
        "error should include requested ({total_wanted}) and RLIMIT_NOFILE ({}): {msg}",
        rlim.rlim_cur
    );
}

/// `T2-F09-4`: Pool spans > 1 GiB split cleanly across whole buffers without straddling 1 GiB.
#[test]
fn t2_f09_4_one_gib_range_boundary_no_straddle() {
    const ONE_GIB: usize = 1usize << 30;
    let buf_sz = 200 * 1024 * 1024u32; // 200 MiB -> 5 bufs = 1000 MiB <= 1 GiB
    let limits = Limits {
        sockets_max: 2,
        buffers: 0,
        buffer_size: 0,
        small_buffers: 0,
        small_buffer_size: 0,
        files_max: 2,
        aligned_buffers: 8,
        aligned_buffer_size: buf_sz,
        block_size: 4096,
        ops_max: 2,
        ..Limits::BASE
    };
    assert_eq!(limits.buffer_count(), 8);
    assert!(limits.pool_bytes() > ONE_GIB);

    // Group contiguous buffers into <= 1 GiB registration chunks without splitting any buffer.
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let (mut cur_start, mut cur_len) = (0usize, 0usize);
    for i in 0..limits.aligned_buffers {
        let (off, len) = limits.span(limits.aligned(i));
        assert_eq!(len, buf_sz as usize);
        assert!(len <= ONE_GIB);
        if cur_len == 0 {
            cur_start = off;
            cur_len = len;
        } else if off == cur_start + cur_len && cur_len + len <= ONE_GIB {
            cur_len += len;
        } else {
            chunks.push((cur_start, cur_len));
            cur_start = off;
            cur_len = len;
        }
    }
    if cur_len > 0 {
        chunks.push((cur_start, cur_len));
    }
    assert_eq!(
        chunks.len(),
        2,
        "8 x 200 MiB buffers fit in two <=1 GiB chunks"
    );
    assert_eq!(chunks[0].1, 5 * buf_sz as usize);
    assert_eq!(chunks[1].1, 3 * buf_sz as usize);
    assert!(chunks.iter().all(|&(_, len)| len <= ONE_GIB));
}

/// `T2-F09-5`: Indexing `Limits::span` / `Io::buf` at `buffer_count()` panics cleanly.
#[test]
#[should_panic(expected = "buffer out of range")]
fn t2_f09_5_span_at_buffer_count_boundary_panics() {
    let limits = file_limits(2, 3, 4096, 4096);
    let oob = BufId(limits.buffer_count());
    let _: (usize, usize) = limits.span(oob);
}
