// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 13: `file_uring` real-kernel check & direct-IO bench wiring.

use super::helpers::repo_root;
#[cfg(target_os = "linux")]
use crate::e2e::harness::{file_uring_bin, TempDir};

#[cfg(target_os = "linux")]
fn run_file_uring(args: &[&str]) -> std::process::Output {
    std::process::Command::new(file_uring_bin())
        .args(args)
        .output()
        .expect("execute file_uring")
}

#[cfg(target_os = "linux")]
fn run_file_uring_in_temp(prefix: &str) -> std::process::Output {
    let dir = TempDir::new_in_test_dir(prefix).expect("temp dir");
    run_file_uring(&["--dir", &dir.path().to_string_lossy()])
}

/// `T1-F13-1`: (`Linux`) `file_uring` executes real-kernel `O_DIRECT` file ops & chains and exits 0.
#[cfg(target_os = "linux")]
#[test]
fn t1_f13_1_file_uring_binary_executes_o_direct() {
    let out = run_file_uring_in_temp("e2e_f13_direct");
    assert!(
        out.status.success(),
        "file_uring failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `T1-F13-2`: (`Linux`) `file_uring` runs under `alloc_guard` sealed after setup without aborting.
#[cfg(target_os = "linux")]
#[test]
fn t1_f13_2_file_uring_alloc_guard_sealed() {
    let out = run_file_uring_in_temp("e2e_f13_sealed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("alloc_guard:"),
        "alloc_guard tripped in file_uring: {stderr}"
    );
    assert!(out.status.success(), "file_uring exited {}", out.status);
}

/// `T1-F13-3`: (`Linux`) `file_uring` exercises >16,384 registered buffers on Linux kernel.
#[cfg(target_os = "linux")]
#[test]
fn t1_f13_3_file_uring_over_16384_buffers() {
    let out = run_file_uring_in_temp("e2e_f13_16k");
    assert!(out.status.success(), "file_uring failed: {:?}", out.status);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("ok file_uring")
            || combined.contains("16384")
            || combined.contains("buffers"),
        "expected file_uring summary output, got: {combined}"
    );
}

/// `T1-F13-4`: (`Linux`) `file_uring` exercises `IORING_SETUP_SUBMIT_ALL` partial-refusal batching.
#[cfg(target_os = "linux")]
#[test]
fn t1_f13_4_file_uring_submit_all_partial_refusal() {
    let out = run_file_uring_in_temp("e2e_f13_submit_all");
    assert!(out.status.success(), "file_uring failed: {:?}", out.status);
}

/// `T1-F13-5`: `scripts/test-linux.sh`, CI `uring` job, and `bench/results/` include `file_uring` & direct-IO records.
#[test]
fn t1_f13_5_scripts_ci_and_bench_results_wired() {
    let root = repo_root();
    let test_linux =
        std::fs::read_to_string(root.join("scripts/test-linux.sh")).expect("read test-linux.sh");
    assert!(
        test_linux.contains("file_uring"),
        "scripts/test-linux.sh must build and execute file_uring"
    );
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    assert!(
        ci.contains("file_uring"),
        ".github/workflows/ci.yml uring job must run file_uring"
    );
    let results_dir = root.join("bench/results");
    assert!(results_dir.is_dir(), "bench/results directory missing");
    let entries: Vec<String> = std::fs::read_dir(&results_dir)
        .expect("read bench/results")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries
            .iter()
            .any(|name| name.contains("file") || name.contains("s3")),
        "expected file/S3 benchmark records under bench/results/"
    );
}
