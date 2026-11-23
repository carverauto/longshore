// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `getrlimit` (`RLIMIT_NOFILE` and `RLIMIT_MEMLOCK`) checks for `file_uring`.

use longshore::io::sys::{getrlimit, rlimit, RLIMIT_MEMLOCK, RLIMIT_NOFILE};
use longshore::io::uring::UringIo;
use longshore::io::Limits;

#[repr(C)]
struct SetRlimit {
    rlim_cur: u64,
    rlim_max: u64,
}

extern "C" {
    fn setrlimit(resource: i32, rlim: *const SetRlimit) -> i32;
    fn geteuid() -> u32;
    fn setuid(uid: u32) -> i32;
    fn setgid(gid: u32) -> i32;
}

pub(super) fn check_rlimit_nofile() {
    let mut rlim = rlimit::default();
    assert_eq!(unsafe { getrlimit(RLIMIT_NOFILE, &mut rlim) }, 0);
    let wanted = rlim.rlim_cur.saturating_add(16).min(u64::from(u32::MAX)) as u32;
    let sockets_max = wanted / 2;
    let files_max = wanted - sockets_max;
    let limits = Limits {
        sockets_max,
        files_max,
        buffers: 1,
        buffer_size: 4096,
        ops_max: 2,
        ..Limits::BASE
    };
    let err = match UringIo::new(limits) {
        Ok(_) => panic!("expected RLIMIT_NOFILE failure"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains(&wanted.to_string()) && msg.contains(&rlim.rlim_cur.to_string()),
        "RLIMIT_NOFILE error must include {wanted} and {}: {msg}",
        rlim.rlim_cur
    );
}

pub(super) fn check_memlock_in_child() {
    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .arg("--memlock-child")
        .output()
        .expect("spawn memlock child");
    assert!(
        out.status.success(),
        "memlock child failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub(super) fn run_memlock_child() {
    if unsafe { geteuid() } == 0 {
        assert_eq!(unsafe { setgid(65534) }, 0, "setgid nobody");
        assert_eq!(unsafe { setuid(65534) }, 0, "setuid nobody");
    }
    let mut orig = rlimit::default();
    assert_eq!(
        unsafe { getrlimit(RLIMIT_MEMLOCK, &mut orig) },
        0,
        "getrlimit"
    );

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
        let lim = SetRlimit {
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
        Ok(_) => panic!("expected RLIMIT_MEMLOCK OutOfMemory for cap={cap}"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::OutOfMemory);
    let msg = err.to_string();
    assert!(msg.contains("RLIMIT_MEMLOCK"), "{msg}");
    assert!(msg.contains(&format!("{cap} bytes")), "{msg}");
    assert!(msg.contains(&limits.pool_bytes().to_string()), "{msg}");
    assert!(msg.contains("shared across all rings"), "{msg}");
}
