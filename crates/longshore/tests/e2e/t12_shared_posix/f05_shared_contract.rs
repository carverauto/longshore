// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 E2E tests for Feature 5: shared `Io` contract suite & `test-linux.sh` (`#41`).

#[cfg(unix)]
use crate::e2e::harness::drive_until;
#[cfg(unix)]
use crate::e2e::harness::posix_backend;
use longshore::io::contract;
use longshore::io::scenario::echo;
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{Completions, Io, Limits};
#[cfg(unix)]
use longshore::io::{Op, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;

pub(super) fn workspace_root() -> PathBuf {
    crate::e2e::harness::workspace_root()
}

pub(super) fn for_each_backend(
    mut run: impl FnMut(&mut dyn FnMut(Limits) -> Box<dyn Io>, SocketAddr),
) {
    #[cfg(feature = "sim")]
    {
        let sim_addr: SocketAddr = "192.0.2.50:443".parse().unwrap();
        run(
            &mut |l| Box::new(SimIo::new(l, 11, Faults::default())),
            sim_addr,
        );
    }
    #[cfg(unix)]
    run(
        &mut |l| Box::new(posix_backend(l).expect("posix")),
        "127.0.0.1:0".parse().unwrap(),
    );
    #[cfg(target_os = "linux")]
    run(
        &mut |l| Box::new(UringIo::new(l).expect("uring")),
        "127.0.0.1:0".parse().unwrap(),
    );
}

#[test]
fn t1_f05_1_shared_one_completion_across_backends() {
    #[cfg(feature = "sim")]
    {
        let addr: SocketAddr = "192.0.2.51:443".parse().unwrap();
        contract::one_completion_and_echo(&mut |l| SimIo::new(l, 1, Faults::default()), addr);
    }
    #[cfg(unix)]
    {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        contract::one_completion_and_echo(&mut |l| posix_backend(l).expect("posix"), addr);
    }
    #[cfg(target_os = "linux")]
    {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        contract::one_completion_and_echo(&mut |l| UringIo::new(l).expect("uring"), addr);
    }
}

#[test]
fn t1_f05_2_shared_busy_backpressure_across_backends() {
    #[cfg(feature = "sim")]
    contract::capacity_busy(&mut |l| SimIo::new(l, 2, Faults::default()));
    #[cfg(unix)]
    contract::capacity_busy(&mut |l| posix_backend(l).expect("posix"));
    #[cfg(target_os = "linux")]
    contract::capacity_busy(&mut |l| UringIo::new(l).expect("uring"));
}

#[test]
fn t1_f05_3_shared_timers_and_wake_across_backends() {
    #[cfg(feature = "sim")]
    {
        contract::timers(&mut |l| SimIo::new(l, 3, Faults::default()));
        contract::wake_ready(&mut |l| SimIo::new(l, 3, Faults::default()));
    }
    #[cfg(unix)]
    {
        contract::timers(&mut |l| posix_backend(l).expect("posix"));
        contract::wake_ready(&mut |l| posix_backend(l).expect("posix"));

        let limits = Limits::BASE
            .with_sockets_max(1)
            .with_buffers(1)
            .with_buffer_size(64)
            .with_ops_max(2);
        let mut io = posix_backend(limits).expect("posix");
        let mut out = Completions::new(4);
        let waker = io.waker().expect("waker");
        io.submit(Token::new(1), Op::AwaitWake).unwrap();
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            waker.wake();
        });
        let cs = drive_until(&mut io, &mut out, 100, |v| !v.is_empty()).unwrap();
        assert_eq!((cs[0].token(), cs[0].result()), (Token::new(1), 0));
        h.join().unwrap();
    }
    #[cfg(target_os = "linux")]
    {
        contract::timers(&mut |l| UringIo::new(l).expect("uring"));
        contract::wake_ready(&mut |l| UringIo::new(l).expect("uring"));
    }
}

#[test]
fn t1_f05_4_shared_loopback_echo_short_sends_partial_recvs() {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(3)
        .with_buffer_size(4096)
        .with_ops_max(4);
    #[cfg(feature = "sim")]
    {
        let addr: SocketAddr = "192.0.2.52:443".parse().unwrap();
        let faults = Faults::NONE
            .with_short_send(250_000)
            .with_partial_recv(250_000)
            .with_delay(150_000);
        for zc in [false, true] {
            let mut io = SimIo::new(limits, 19, faults);
            let mut out = Completions::new(8);
            let stats = echo(&mut io, addr, 64 * 1024, 4096, zc, &mut out).expect("sim echo");
            assert!(stats.sends() > 0 && stats.recvs() > 0);
        }
    }
    #[cfg(unix)]
    for zc in [false, true] {
        let mut io = posix_backend(limits).expect("posix");
        let mut out = Completions::new(8);
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let stats = echo(&mut io, addr, 64 * 1024, 4096, zc, &mut out).expect("posix echo");
        assert!(stats.sends() > 0 && stats.recvs() > 0);
    }
    #[cfg(target_os = "linux")]
    for zc in [false, true] {
        let mut io = UringIo::new(limits).expect("uring");
        let mut out = Completions::new(8);
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let stats = echo(&mut io, addr, 64 * 1024, 4096, zc, &mut out).expect("uring echo");
        assert!(stats.sends() > 0 && stats.recvs() > 0);
    }
}

#[test]
fn t1_f05_5_test_linux_script_and_ci_wire_contract_uring() {
    let root = workspace_root();
    let script_path = root.join("scripts/test-linux.sh");
    let script = std::fs::read_to_string(&script_path).expect("read test-linux.sh");
    assert!(
        script.contains("contract_uring"),
        "scripts/test-linux.sh must build and run contract_uring"
    );

    let syntax = Command::new("sh")
        .args(["-n", script_path.to_str().expect("utf8")])
        .output()
        .expect("sh -n");
    assert!(
        syntax.status.success(),
        "test-linux.sh syntax error: {}",
        String::from_utf8_lossy(&syntax.stderr)
    );

    let ci_path = root.join(".github/workflows/ci.yml");
    let ci = std::fs::read_to_string(&ci_path).expect("read ci.yml");
    assert!(
        ci.contains("contract_uring"),
        "ci.yml must reference contract_uring"
    );
}
