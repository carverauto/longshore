// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The allocation guard does what every "does not allocate" test in the
//! workspace assumes: it ends the process when a sealed thread touches the
//! heap, and only then.
//!
//! A violation aborts, so each case runs in a child: this test binary
//! started again, running only [`child`], with the case named in the
//! environment.

use longshore_common::io::alloc_guard::{allow_alloc, sealed, Guard};
use std::hint::black_box;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};

#[global_allocator]
static ALLOC: Guard = Guard;

const CASE: &str = "LONGSHORE_GUARD_CASE";
const SIGABRT: i32 = 6;
const PANIC_MESSAGE: &str = "the panic's own message";

/// Not a test of its own: the body of each case, run in a child process.
#[test]
fn child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    match case.as_str() {
        "alloc" => drop(sealed(|| black_box(Vec::<u8>::with_capacity(64)))),
        "free" => {
            let v = black_box(vec![0u8; 64]);
            sealed(move || drop(v));
        }
        "realloc" => {
            let mut v = black_box(Vec::<u8>::with_capacity(1));
            sealed(|| v.reserve(4096));
        }
        "allowed" => sealed(|| allow_alloc(|| drop(black_box(vec![0u8; 64])))),
        "resealed" => drop(sealed(|| {
            allow_alloc(|| drop(black_box(vec![0u8; 64])));
            black_box(vec![0u8; 8])
        })),
        "nested-allowed" => sealed(|| {
            // An exception inside an exception ends where it began.
            allow_alloc(|| allow_alloc(|| ()));
            allow_alloc(|| drop(black_box(vec![0u8; 64])));
        }),
        "afterwards" => {
            assert_eq!(sealed(|| 7), 7);
            drop(black_box(vec![0u8; 64]));
        }
        "other-thread" => {
            // The seal is this thread's alone.
            static STOP: AtomicBool = AtomicBool::new(false);
            static RUNNING: AtomicBool = AtomicBool::new(false);
            let other = std::thread::spawn(|| {
                let mut made = 0u64;
                while !STOP.load(Ordering::Acquire) {
                    drop(black_box(vec![0u8; 64]));
                    made += 1;
                    RUNNING.store(true, Ordering::Release);
                }
                made
            });
            sealed(|| {
                while !RUNNING.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                RUNNING.store(false, Ordering::Release);
                while !RUNNING.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                STOP.store(true, Ordering::Release);
            });
            assert!(other.join().unwrap() > 0);
        }
        "panic" => sealed(|| panic!("{PANIC_MESSAGE}")),
        "panic-formatted" => sealed(|| {
            let got: Result<u32, String> = black_box(Err(String::new()));
            assert_eq!(got, Ok(1), "{PANIC_MESSAGE}");
        }),
        "panic-drop-allocates" => sealed(|| {
            struct AllocatesWhenDropped;
            impl Drop for AllocatesWhenDropped {
                fn drop(&mut self) {
                    drop(black_box(Vec::<u8>::with_capacity(64)));
                }
            }
            let _guard = AllocatesWhenDropped;
            panic!("{PANIC_MESSAGE}");
        }),
        "panic-caught" => {
            let caught = std::panic::catch_unwind(|| sealed(|| panic!("{PANIC_MESSAGE}")));
            assert!(caught.is_err());
            // The seal ended with the region it covered.
            drop(black_box(vec![0u8; 64]));
        }
        "panic-caught-then-sealed" => {
            let caught = std::panic::catch_unwind(|| sealed(|| panic!("{PANIC_MESSAGE}")));
            assert!(caught.is_err());
            // And the guard still works afterwards.
            drop(sealed(|| black_box(vec![0u8; 8])));
        }
        "panic-in-allowed" => {
            let caught = std::panic::catch_unwind(|| {
                sealed(|| allow_alloc(|| panic!("{PANIC_MESSAGE}")));
            });
            assert!(caught.is_err());
            drop(black_box(vec![0u8; 64]));
        }
        other => panic!("unknown case {other:?}"),
    }
}

/// Run `case` in a child, with the test harness capturing its output or not.
#[cfg(test)]
fn run(case: &str, capture: bool) -> (Output, String) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    let _: &mut Command = cmd.args(["--exact", "child", "--test-threads", "1"]);
    if !capture {
        let _: &mut Command = cmd.arg("--nocapture");
    }
    let out = cmd.env(CASE, case).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

fn assert_aborts(case: &str, what: &str) {
    for capture in [true, false] {
        let (out, text) = run(case, capture);
        assert_eq!(
            out.status.signal(),
            Some(SIGABRT),
            "{case} (capture {capture}): {:?}\n{text}",
            out.status
        );
        let message = format!("alloc_guard: heap {what} while sealed");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&message),
            "{case} (capture {capture}): no {message:?} in\n{text}"
        );
    }
}

fn assert_passes(case: &str) {
    for capture in [true, false] {
        let (out, text) = run(case, capture);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{case} (capture {capture}): {:?}\n{text}",
            out.status
        );
        assert!(text.contains("1 passed"), "{case}: the child ran\n{text}");
        assert!(!text.contains("alloc_guard"), "{case}:\n{text}");
    }
}

#[test]
fn a_child_without_a_case_does_nothing() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child"])
        .env_remove(CASE)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn an_allocation_while_sealed_aborts_and_says_so() {
    assert_aborts("alloc", "alloc");
}

#[test]
fn a_free_while_sealed_aborts_and_says_so() {
    assert_aborts("free", "free");
}

#[test]
fn a_reallocation_while_sealed_aborts_and_says_so() {
    assert_aborts("realloc", "realloc");
}

#[test]
fn the_exception_permits_allocation_inside_a_sealed_region() {
    assert_passes("allowed");
    assert_passes("nested-allowed");
}

#[test]
fn the_seal_is_back_once_the_exception_ends() {
    assert_aborts("resealed", "alloc");
}

#[test]
fn the_seal_ends_with_its_region_and_is_per_thread() {
    assert_passes("afterwards");
    assert_passes("other-thread");
}

/// A test that fails inside a sealed region must say why it failed. The
/// panic machinery allocates; without care that reads as a violation, and
/// the process aborts with the guard's message in place of the assertion's.
#[test]
fn a_panic_while_sealed_is_reported_as_itself() {
    for case in ["panic", "panic-formatted"] {
        for capture in [true, false] {
            let (out, text) = run(case, capture);
            assert!(
                text.contains(PANIC_MESSAGE),
                "{case} (capture {capture}): the panic's message is missing from\n{text}"
            );
            assert!(
                !text.contains("alloc_guard"),
                "{case} (capture {capture}): reported as a violation:\n{text}"
            );
            assert_eq!(
                (out.status.signal(), out.status.code()),
                (None, Some(101)),
                "{case} (capture {capture}): a failed test, not an abort\n{text}"
            );
        }
    }
}

/// The exemption covers the whole unwind, not only the report: a destructor
/// that allocates while a panic unwinds a sealed region is not a violation
/// either. This is the rule, written down so that it is a choice. The
/// guard cannot tell the report's allocations from a destructor's, since
/// both happen while the thread is panicking; the test has failed already
/// by then, with the panic's own message; and what ships is built with
/// `panic = "abort"`, where nothing unwinds at all.
#[test]
fn a_destructor_that_allocates_during_the_unwind_is_exempt_too() {
    for capture in [true, false] {
        let (out, text) = run("panic-drop-allocates", capture);
        assert!(text.contains(PANIC_MESSAGE), "capture {capture}:\n{text}");
        assert!(!text.contains("alloc_guard"), "capture {capture}:\n{text}");
        assert_eq!(
            (out.status.signal(), out.status.code()),
            (None, Some(101)),
            "capture {capture}: a failed test, not an abort\n{text}"
        );
    }
}

/// A panic that leaves a sealed region and is caught leaves the thread
/// unsealed, and the guard working.
#[test]
fn a_caught_panic_ends_the_seal_it_passed_through() {
    assert_passes("panic-caught");
    assert_passes("panic-in-allowed");
    assert_aborts("panic-caught-then-sealed", "alloc");
}
