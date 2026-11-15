// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Write one object only if it is absent, then prove what was written:
//! `put` with a SHA-256 and encryption asked for, `head` and two `get`s
//! bound to the identity the store returned, each one request within one
//! deadline. Linux only, against a real S3 store:
//!
//! ```text
//! LONGSHORE_HOST=my-bucket.s3.us-east-1.amazonaws.com LONGSHORE_REGION=us-east-1 \
//! AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... \
//!     cargo run -p longshore_s3 --example verified_write -- some/new/key
//! ```
//!
//! Set `LONGSHORE_BUCKET` as well for a store that takes the bucket in the
//! path, with `LONGSHORE_HOST` as the endpoint's own name.

use longshore_s3::client::{Budget, Builder, Client};
use longshore_s3::creds::Credentials;
use longshore_s3::crypto::sha256;
use longshore_s3::report::{Answer, Encryption, End, Expect, Put, Report, ServerEncryption};
use longshore_s3::s3::Checksum;
use std::time::{Duration, Instant};

type Failure = Box<dyn std::error::Error>;

fn main() -> Result<(), Failure> {
    let var = |name: &str| std::env::var(name).map_err(|_| format!("set {name}"));
    let key = std::env::args().nth(1).ok_or("give the key to write")?;
    let credentials = match var("AWS_SESSION_TOKEN") {
        Ok(token) => Credentials::with_session(
            &var("AWS_ACCESS_KEY_ID")?,
            &var("AWS_SECRET_ACCESS_KEY")?,
            &token,
        ),
        Err(_) => Credentials::new(&var("AWS_ACCESS_KEY_ID")?, &var("AWS_SECRET_ACCESS_KEY")?),
    }
    .ok_or("the credentials are not usable")?;
    let mut settings = Builder::new(
        &var("LONGSHORE_HOST")?,
        &var("LONGSHORE_REGION")?,
        credentials,
    );
    settings.bucket_in_path = var("LONGSHORE_BUCKET").ok();
    (settings.tls, settings.port) = (true, 443);
    // One operation at a time needs little: a megabyte for the object, and
    // what two connections use.
    (settings.connections, settings.buffers, settings.operations) = (2, 2, 2);
    settings.buffer_size = 1 << 20;
    let client = settings.build()?;

    // Everything below shares one deadline, and each step is one request.
    let budget = Budget::once(Instant::now() + Duration::from_secs(30));
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let digest = sha256(&bytes);

    let mut body = client.buffer_blocking();
    body.as_mut_slice()[..bytes.len()].copy_from_slice(&bytes);
    let options = Put {
        no_clobber: true,
        encryption: Some(Encryption::Aes256),
        checksum: Checksum::Sha256(digest),
    };
    let (put, body) = client.put(&key, body, bytes.len(), &options, budget).wait();
    let put = put.map_err(|e| format!("nothing was sent: {e:?}"))?;
    let signed = put.request.ok_or("nothing was sent")?;
    println!(
        "sent {} at {} signing {}",
        signed.method,
        String::from_utf8_lossy(&signed.date),
        String::from_utf8_lossy(signed.signed_headers.as_bytes()),
    );
    let stored = answered(&put, 200, "the write")?;
    println!(
        "stored: etag {:?} version {:?} encryption {:?} sha256 returned: {}",
        stored.etag,
        stored.version,
        stored.encryption,
        stored.sha256.is_some(),
    );
    // What the store did not say is not assumed.
    if stored.encryption != Some(ServerEncryption::Aes256) {
        return Err("the store did not confirm the encryption asked for".into());
    }
    let expect = Expect {
        etag: Some(stored.etag.ok_or("the store returned no ETag")?),
        // A bucket without versioning answers nothing, or "null".
        version: stored.version.filter(|v| v.as_bytes() != b"null"),
    };

    let head = client.head(&key, &expect, budget).wait();
    let head = head.map_err(|e| format!("nothing was sent: {e:?}"))?;
    let described = answered(&head, 200, "the head")?;
    if described.content_length != Some(bytes.len() as u64) || described.etag != expect.etag {
        return Err("the store describes some other object".into());
    }

    let body = read(&client, &key, None, &expect, body, budget, &bytes)?;
    read(
        &client,
        &key,
        Some((1000, 1999)),
        &expect,
        body,
        budget,
        &bytes,
    )?;
    println!("read back whole and in part: {} bytes match", bytes.len());
    Ok(())
}

/// The answer of a report, if the store gave one with status `status`.
fn answered(report: &Report, status: u16, what: &str) -> Result<Answer, Failure> {
    match report.end {
        End::Answered(answer) if answer.status == status => Ok(answer),
        End::Answered(answer) => Err(format!("{what} was answered {}", answer.status).into()),
        End::NotSent(cause) => Err(format!("{what} was not sent: {cause:?}").into()),
        // Sent and not answered. For a write this is not a failure to
        // write: the object may be there.
        End::Unknown(cause) => Err(format!("{what} has an unknown outcome: {cause:?}").into()),
    }
}

/// Fetch `range` of the object and compare it with the same bytes of `sent`.
fn read(
    client: &Client,
    key: &str,
    range: Option<(u64, u64)>,
    expect: &Expect,
    into: longshore_s3::client::Buffer,
    budget: Budget,
    sent: &[u8],
) -> Result<longshore_s3::client::Buffer, Failure> {
    let (got, into) = client.get(key, range, expect, into, budget).wait();
    let got = got.map_err(|e| format!("nothing was sent: {e:?}"))?;
    let (first, last) = range.unwrap_or((0, sent.len() as u64 - 1));
    let answer = answered(&got, if range.is_some() { 206 } else { 200 }, "the read")?;
    // A ranged answer must say it carries exactly the range asked for.
    let served = answer.content_range.map(|r| (r.first, r.last, r.size));
    if range.is_some() && served != Some((first, last, Some(sent.len() as u64))) {
        return Err(format!("the store served {served:?}").into());
    }
    if answer.etag != expect.etag
        || into.as_slice()[..answer.body_len as usize] != sent[first as usize..=last as usize]
    {
        return Err("the bytes read are not the bytes written".into());
    }
    Ok(into)
}
