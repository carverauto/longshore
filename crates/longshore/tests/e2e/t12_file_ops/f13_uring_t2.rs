// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 2 E2E tests for Feature 13: `file_uring` limits, diagnostics, and failure checks.

use super::helpers::repo_root;

#[cfg(target_os = "linux")]
fn file_uring_bin() -> std::path::PathBuf {
    let root = repo_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = std::process::Command::new(&cargo)
        .current_dir(&root)
        .args([
            "build",
            "--quiet",
            "-p",
            "longshore-bench",
            "--bin",
            "file_uring",
        ])
        .output()
        .expect("build file_uring");
    assert!(build.status.success());
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("target"));
    target_dir.join("debug/file_uring")
}

/// `T2-F13-1`: (`Linux`) `file_uring` verifies lowered `RLIMIT_MEMLOCK` failure in child process.
#[cfg(target_os = "linux")]
#[test]
fn t2_f13_1_file_uring_lowered_memlock_child() {
    let out = std::process::Command::new(file_uring_bin())
        .output()
        .expect("run file_uring");
    assert!(
        out.status.success(),
        "file_uring memlock child check failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `T2-F13-2`: (`Linux`) `file_uring` checks `RLIMIT_NOFILE` boundary (`sockets_max + files_max`).
#[cfg(target_os = "linux")]
#[test]
fn t2_f13_2_file_uring_rlimit_nofile_boundary() {
    let out = std::process::Command::new(file_uring_bin())
        .output()
        .expect("run file_uring");
    assert!(out.status.success());
}

/// `T2-F13-3`: (`Linux`) `file_uring` refuses misaligned `O_DIRECT` write with `-EINVAL`.
#[cfg(target_os = "linux")]
#[test]
fn t2_f13_3_file_uring_misaligned_o_direct_einval() {
    let out = std::process::Command::new(file_uring_bin())
        .output()
        .expect("run file_uring");
    assert!(out.status.success());
}

/// `T2-F13-4`: (`Linux`) `file_uring` rejects nonexistent/invalid `--dir` with explicit diagnostic.
#[cfg(target_os = "linux")]
#[test]
fn t2_f13_4_file_uring_non_direct_or_missing_dir_diagnostic() {
    let out = std::process::Command::new(file_uring_bin())
        .args(["--dir", "/nonexistent/longshore_direct_path"])
        .output()
        .expect("run file_uring");
    assert!(
        !out.status.success(),
        "invalid directory must fail explicitly"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.trim().is_empty(),
        "expected clear diagnostic on stderr"
    );
}

/// `T2-F13-5`: Unknown `file_uring` flag and `test-linux.sh` with missing hosts fail non-zero.
#[test]
fn t2_f13_5_bad_flag_and_test_linux_missing_host_fail() {
    let root = repo_root();
    let out = std::process::Command::new("sh")
        .current_dir(&root)
        .arg("scripts/test-linux.sh")
        .env(
            "LONGSHORE_TEST_HOSTS_FILE",
            "/nonexistent/longshore_missing_hosts",
        )
        .output()
        .expect("run test-linux.sh");
    assert!(
        !out.status.success(),
        "test-linux.sh must fail non-zero when hosts file is missing"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("io_uring was not exercised") || err.contains("not found"),
        "expected explicit error on stderr, got: {err}"
    );

    #[cfg(target_os = "linux")]
    {
        let u = std::process::Command::new(file_uring_bin())
            .arg("--definitely-invalid-flag")
            .output()
            .expect("run file_uring bad flag");
        assert!(!u.status.success(), "unknown flag must exit non-zero");
    }
}
