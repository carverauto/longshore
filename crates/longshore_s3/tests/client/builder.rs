// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Settings an I/O thread could not start with are refused at build, on
//! every platform, before any thread exists.

use longshore_s3::client::{BuildError, Builder, TlsProvider};
use longshore_s3::creds::Credentials;
use std::net::SocketAddr;

#[cfg(test)]
fn settings(host: &str) -> Builder {
    let credentials = Credentials::new("AKIDEXAMPLE", "secret").unwrap();
    Builder::new(host, "us-east-1", credentials).nameservers(vec!["192.0.2.53:53".parse().unwrap()])
}

fn refused(b: Builder) -> bool {
    matches!(b.build(), Err(BuildError::Setting(_)))
}

#[test]
fn a_region_longer_than_64_bytes_is_refused() {
    let b = settings("bucket.s3.example.com").region(&"r".repeat(65));
    assert!(refused(b));
}

#[test]
fn more_than_two_nameservers_are_refused() {
    let b = settings("bucket.s3.example.com").nameservers(
        (1..=3)
            .map(|n| SocketAddr::from(([192, 0, 2, n], 53)))
            .collect(),
    );
    assert!(refused(b));
}

#[test]
fn a_host_dns_cannot_look_up_is_refused_unless_an_address_is_given() {
    for host in [
        "bucket..example.com",
        "bucket example.com",
        "bücket.example",
    ] {
        assert!(refused(settings(host)), "{host:?}");
        let fixed = settings(host).address(Some("192.0.2.1:80".parse().unwrap()));
        assert!(!refused(fixed), "{host:?} with a fixed address");
    }
}

#[test]
fn ordinary_settings_pass_the_checks() {
    let b = settings("bucket.s3.example.com")
        .region(&"r".repeat(64))
        .nameservers(vec![
            "192.0.2.53:53".parse().unwrap(),
            "192.0.2.54:53".parse().unwrap(),
        ]);
    assert!(!refused(b));
}

#[test]
fn a_credentials_file_must_be_an_absolute_path() {
    let relative =
        settings("bucket.s3.example.com").credentials_file(Some("relative/credentials".into()));
    assert!(refused(relative));
    let absolute = settings("bucket.s3.example.com")
        .credentials_file(Some("/etc/longshore/credentials".into()));
    assert!(!refused(absolute));
}

#[test]
fn a_bucket_in_the_path_must_be_a_bucket_name() {
    for bad in [
        "ab",
        "Has-Capitals",
        "with space",
        "with/slash",
        &"x".repeat(64),
    ] {
        let b = settings("s3.example.com").bucket_in_path(Some(bad.into()));
        assert!(refused(b), "{bad:?}");
    }
    let b = settings("s3.example.com").bucket_in_path(Some("my.bucket-1".into()));
    assert!(!refused(b));
}

#[cfg(not(feature = "tls-core"))]
#[test]
fn tls_without_the_feature_is_refused_with_a_setting_error_naming_tls() {
    let b = settings("bucket.s3.example.com").tls(true);
    match b.build() {
        Err(BuildError::Setting(msg)) => {
            assert!(msg.contains("tls"), "setting error must name tls: {msg}");
        }
        _ => panic!("expected BuildError::Setting when tls feature is off"),
    }
}

#[test]
fn a_tls_provider_that_is_not_compiled_in_is_refused_by_its_feature_name() {
    let all = [TlsProvider::AwsLc, TlsProvider::Ring, TlsProvider::Rustls];
    for (p, feature) in all.into_iter().zip(["aws-lc", "ring", "rustls"]) {
        let b = settings("bucket.s3.example.com").tls(true).tls_provider(p);
        match b.build() {
            Err(BuildError::Setting(msg)) => {
                assert!(!p.compiled(), "{p:?} is compiled in but refused: {msg}");
                assert!(
                    msg.contains(feature),
                    "the error must name `{feature}`: {msg}"
                );
            }
            _ => assert!(p.compiled(), "{p:?} is not compiled in but was accepted"),
        }
    }
}

#[test]
fn the_default_tls_provider_is_the_first_compiled_in_of_aws_lc_ring_and_rustls() {
    let expected = [TlsProvider::AwsLc, TlsProvider::Ring, TlsProvider::Rustls]
        .into_iter()
        .find(|p| p.compiled());
    assert_eq!(TlsProvider::preferred(), expected);
    // Without a provider chosen, TLS builds exactly when one is compiled in.
    let b = settings("bucket.s3.example.com").tls(true);
    assert_eq!(refused(b), expected.is_none());
}

#[test]
fn builder_validation_edges_and_non_utf8_credentials_and_partial_start_failure() {
    let no_operations = settings("bucket.s3.example.com").operations(0);
    assert!(refused(no_operations));

    let no_buffers = settings("bucket.s3.example.com").buffers(0);
    assert!(refused(no_buffers));

    let small_buffers = settings("bucket.s3.example.com").buffer_size(8191);
    assert!(refused(small_buffers));

    let long_host = settings(&"a".repeat(254)).address(Some("192.0.2.1:80".parse().unwrap()));
    assert!(refused(long_host));

    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let non_utf8 = settings("bucket.s3.example.com")
            .credentials_file(Some(PathBuf::from(OsStr::from_bytes(b"/etc/\xff/creds"))));
        assert!(refused(non_utf8));
    }

    // Subprocess partial multi-thread start failure cleanup under lowered RLIMIT_NOFILE.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if std::env::var_os(PARTIAL_START).is_some() {
            partial_start_fails_with_io_error();
            return;
        }
        rerun_with_nofile_limit(
            "builder_validation_edges_and_non_utf8_credentials_and_partial_start_failure",
            24,
        );
    }
}

/// Set in the child that runs the partial start.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const PARTIAL_START: &str = "LONGSHORE_S3_PARTIAL_START";

/// 32 threads cannot all start under the child's file limit; the build
/// fails with an I/O error once some have started.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn partial_start_fails_with_io_error() {
    let partial = settings("bucket.s3.example.com")
        .address(Some("127.0.0.1:1".parse().unwrap()))
        .threads(32)
        .connections(1)
        .buffers(1)
        .operations(1)
        .waiters(1)
        .portable(true);
    assert!(matches!(partial.build(), Err(BuildError::Io(_))));
}

/// Runs `test` again in a child with `PARTIAL_START` set and
/// `RLIMIT_NOFILE` lowered to `nofile`, and asserts it passes.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rerun_with_nofile_limit(test: &str, nofile: u64) {
    use std::os::unix::process::CommandExt;
    #[cfg(target_os = "linux")]
    const RLIMIT_NOFILE: i32 = 7;
    #[cfg(target_os = "macos")]
    const RLIMIT_NOFILE: i32 = 8;
    #[repr(C)]
    struct Rlimit {
        rlim_cur: u64,
        rlim_max: u64,
    }
    extern "C" {
        fn setrlimit(resource: i32, rlim: *const Rlimit) -> i32;
    }
    let exe = std::env::current_exe().unwrap();
    let mut cmd = std::process::Command::new(exe);
    let _: &mut std::process::Command = cmd.arg(test).arg("--exact").env(PARTIAL_START, "1");
    unsafe {
        let _: &mut std::process::Command = cmd.pre_exec(move || {
            let lim = Rlimit {
                rlim_cur: nofile,
                rlim_max: nofile,
            };
            if setrlimit(RLIMIT_NOFILE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "partial start child failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
