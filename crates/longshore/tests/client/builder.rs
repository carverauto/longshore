// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Settings an I/O thread could not start with are refused at build, on
//! every platform, before any thread exists.

use longshore::client::{BuildError, Builder};
use longshore::store::creds::Credentials;
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
