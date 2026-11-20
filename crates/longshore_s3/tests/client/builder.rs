// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Settings an I/O thread could not start with are refused at build, on
//! every platform, before any thread exists.

use longshore_s3::client::{BuildError, Builder};
use longshore_s3::creds::Credentials;
use std::net::SocketAddr;

fn settings(host: &str) -> Builder {
    let credentials = Credentials::new("AKIDEXAMPLE", "secret").unwrap();
    let mut b = Builder::new(host, "us-east-1", credentials);
    b.nameservers = vec!["192.0.2.53:53".parse().unwrap()];
    b
}

fn refused(b: Builder) -> bool {
    matches!(b.build(), Err(BuildError::Setting(_)))
}

#[test]
fn a_region_longer_than_64_bytes_is_refused() {
    let mut b = settings("bucket.s3.example.com");
    b.region = "r".repeat(65);
    assert!(refused(b));
}

#[test]
fn more_than_two_nameservers_are_refused() {
    let mut b = settings("bucket.s3.example.com");
    b.nameservers = (1..=3)
        .map(|n| SocketAddr::from(([192, 0, 2, n], 53)))
        .collect();
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
        let mut fixed = settings(host);
        fixed.address = Some("192.0.2.1:80".parse().unwrap());
        assert!(!refused(fixed), "{host:?} with a fixed address");
    }
}

#[test]
fn ordinary_settings_pass_the_checks() {
    let mut b = settings("bucket.s3.example.com");
    b.region = "r".repeat(64);
    b.nameservers.push("192.0.2.54:53".parse().unwrap());
    assert!(!refused(b));
}

#[test]
fn a_credentials_file_must_be_an_absolute_path() {
    let mut b = settings("bucket.s3.example.com");
    b.credentials_file = Some("relative/credentials".into());
    assert!(refused(b));
    let mut b = settings("bucket.s3.example.com");
    b.credentials_file = Some("/etc/longshore/credentials".into());
    assert!(!refused(b));
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
        let mut b = settings("s3.example.com");
        b.bucket_in_path = Some(bad.into());
        assert!(refused(b), "{bad:?}");
    }
    let mut b = settings("s3.example.com");
    b.bucket_in_path = Some("my.bucket-1".into());
    assert!(!refused(b));
}

#[cfg(not(feature = "tls"))]
#[test]
fn tls_without_the_feature_is_refused_with_a_setting_error_naming_tls() {
    let mut b = settings("bucket.s3.example.com");
    b.tls = true;
    match b.build() {
        Err(BuildError::Setting(msg)) => {
            assert!(msg.contains("tls"), "setting error must name tls: {msg}");
        }
        _ => panic!("expected BuildError::Setting when tls feature is off"),
    }
}

#[test]
fn builder_validation_edges_and_non_utf8_credentials_and_partial_start_failure() {
    let mut b = settings("bucket.s3.example.com");
    b.operations = 0;
    assert!(refused(b));

    let mut b = settings("bucket.s3.example.com");
    b.buffers = 0;
    assert!(refused(b));

    let mut b = settings("bucket.s3.example.com");
    b.buffer_size = 8191;
    assert!(refused(b));

    let mut b = settings(&"a".repeat(254));
    b.address = Some("192.0.2.1:80".parse().unwrap());
    assert!(refused(b));

    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let mut b = settings("bucket.s3.example.com");
        b.credentials_file = Some(PathBuf::from(OsStr::from_bytes(b"/etc/\xff/creds")));
        assert!(refused(b));
    }

    // Subprocess partial multi-thread start failure cleanup under lowered RLIMIT_NOFILE.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
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
        if std::env::var_os("LONGSHORE_S3_PARTIAL_START").is_some() {
            let mut b = settings("bucket.s3.example.com");
            b.address = Some("127.0.0.1:1".parse().unwrap());
            b.threads = 32;
            b.connections = 1;
            b.buffers = 1;
            b.operations = 1;
            b.waiters = 1;
            #[cfg(target_os = "linux")]
            {
                b.portable = true;
            }
            assert!(matches!(b.build(), Err(BuildError::Io(_))));
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("builder_validation_edges_and_non_utf8_credentials_and_partial_start_failure")
            .arg("--exact")
            .env("LONGSHORE_S3_PARTIAL_START", "1");
        unsafe {
            cmd.pre_exec(|| {
                let lim = Rlimit {
                    rlim_cur: 24,
                    rlim_max: 24,
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
}
