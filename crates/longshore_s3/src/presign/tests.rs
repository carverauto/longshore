// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

// The worked example in Amazon's "Authenticating Requests: Using Query
// Parameters" documentation: 2013-05-24T00:00:00Z, valid for a day.
const EXAMPLE_TIME: u64 = 1_369_353_600;

fn example_creds() -> Credentials {
    Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .unwrap()
}

fn target(creds: &Credentials) -> Target<'_> {
    Target {
        scheme: "https",
        host: "examplebucket.s3.amazonaws.com",
        root: "",
        region: "us-east-1",
        credentials: creds,
    }
}

#[test]
fn matches_the_documented_example() {
    let creds = example_creds();
    let mut out = [0u8; 1024];
    let n = presign(
        &mut out,
        &target(&creds),
        Access::Fetch,
        "test.txt",
        EXAMPLE_TIME,
        86400,
    )
    .unwrap();
    assert_eq!(
        core::str::from_utf8(&out[..n]).unwrap(),
        "https://examplebucket.s3.amazonaws.com/test.txt\
         ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
         &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
         &X-Amz-Date=20130524T000000Z\
         &X-Amz-Expires=86400\
         &X-Amz-SignedHeaders=host\
         &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
    );
}

#[test]
fn a_store_url_and_a_session_token_change_the_signature() {
    let creds = example_creds();
    let mut a = [0u8; 4096];
    let mut b = [0u8; 4096];
    let n = presign(
        &mut a,
        &target(&creds),
        Access::Fetch,
        "k",
        EXAMPLE_TIME,
        60,
    )
    .unwrap();
    let m = presign(
        &mut b,
        &target(&creds),
        Access::Store,
        "k",
        EXAMPLE_TIME,
        60,
    )
    .unwrap();
    assert_eq!(n, m);
    assert_ne!(a[..n], b[..m]);

    let temporary = Credentials::with_session(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "token/with+odd=chars",
    )
    .unwrap();
    let m = presign(
        &mut b,
        &target(&temporary),
        Access::Fetch,
        "k",
        EXAMPLE_TIME,
        60,
    )
    .unwrap();
    let url = core::str::from_utf8(&b[..m]).unwrap();
    assert!(
        url.contains("&X-Amz-Security-Token=token%2Fwith%2Bodd%3Dchars&X-Amz-SignedHeaders=host&")
    );
}

#[test]
fn keys_are_encoded_and_slashes_kept() {
    let creds = example_creds();
    let mut out = [0u8; 1024];
    let n = presign(
        &mut out,
        &target(&creds),
        Access::Fetch,
        "a b/c&d",
        EXAMPLE_TIME,
        60,
    )
    .unwrap();
    let url = core::str::from_utf8(&out[..n]).unwrap();
    assert!(url.starts_with("https://examplebucket.s3.amazonaws.com/a%20b/c%26d?X-Amz-Algorithm="));
}

#[test]
fn refuses_what_a_store_would_refuse_and_what_does_not_fit() {
    let creds = example_creds();
    let t = target(&creds);
    let mut out = [0u8; 1024];
    let call =
        |out: &mut [u8], key, expires| presign(out, &t, Access::Fetch, key, EXAMPLE_TIME, expires);
    assert_eq!(call(&mut out, "", 60), Err(PresignError::BadKey));
    assert_eq!(call(&mut out, "k", 0), Err(PresignError::BadExpiry));
    assert_eq!(
        call(&mut out, "k", EXPIRES_MAX_SECS + 1),
        Err(PresignError::BadExpiry)
    );
    let n = call(&mut out, "k", EXPIRES_MAX_SECS).unwrap();
    assert_eq!(
        call(&mut out[..n - 1], "k", EXPIRES_MAX_SECS),
        Err(PresignError::OutputTooSmall)
    );

    // A bearer token is not a signing key.
    let bearer = Credentials::bearer("ya29.token").unwrap();
    assert_eq!(
        presign(
            &mut out,
            &target(&bearer),
            Access::Fetch,
            "k",
            EXAMPLE_TIME,
            60
        ),
        Err(PresignError::NotSigningCredentials)
    );
}
