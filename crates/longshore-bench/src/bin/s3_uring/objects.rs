// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The object-operations check: store with optional headers, stat, copy,
//! list one level with a delimiter, erase in a batch, and presign a URL.

use super::*;
use longshore::store::erase::{write_manifest, Failures};
use longshore::store::list::Listing;
use longshore::store::options::Options;
use longshore::store::presign::Access;
use longshore::store::stat::header_in;

pub(super) fn objects(size: u32) {
    let gcs = std::env::var("S3_VENDOR").is_ok_and(|v| v == "gcs");
    let (mut io, mut s3, first) = setup(1, 2, size, 2);
    let (tx, rx) = (BufId(first), BufId(first + 1));
    for (i, b) in io.buf(tx)[..size as usize].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[2];
    }
    let base = format!(
        "selftest/objects-{}-{}/",
        std::process::id(),
        io.unix_secs()
    );
    let (a, b) = (format!("{base}a.bin"), format!("{base}sub/b.bin"));
    let options = Options::new()
        .content_type("application/x-longshore-check")
        .and_then(|o| o.metadata("owner", "longshore check"))
        .unwrap_or_else(|| die("options do not fit"));
    let meta_name = if gcs {
        "x-goog-meta-owner"
    } else {
        "x-amz-meta-owner"
    };
    let mut out = Completions::new(32);
    let mut url = [0u8; 2048];
    let t0 = io.now_ns();

    // Sealed: every request below, including DNS, signing and retries.
    let mut lines: Vec<(&str, bool, String)> = Vec::with_capacity(16);
    let mut results = [(false, None); 8];
    let url_len = sealed(|| {
        let mut step = |k: usize, ok: bool, d: Done| results[k] = (ok, Some(d));

        s3.store_with(&mut io, 0, &a, tx, size, Checksum::Compute, &options)
            .unwrap();
        let d = wait(&mut io, &mut s3, &mut out);
        step(0, d.outcome == Outcome::Stored, d);

        s3.stat(&mut io, 1, &a, Some(rx)).unwrap();
        let d = wait(&mut io, &mut s3, &mut out);
        let ok = match d.outcome {
            Outcome::Stat(stat) => {
                let head = &io.buf(rx)[..stat.head_len as usize];
                stat.size == size as u64
                    && stat.tag.is_some()
                    && header_in(head, "content-type") == Some(b"application/x-longshore-check")
                    && header_in(head, meta_name) == Some(b"longshore check")
            }
            _ => false,
        };
        step(1, ok, d);

        s3.copy(&mut io, 2, &a, &b).unwrap();
        let d = wait(&mut io, &mut s3, &mut out);
        step(2, d.outcome == Outcome::Copied, d);

        s3.stat(&mut io, 3, &b, Some(rx)).unwrap();
        let d = wait(&mut io, &mut s3, &mut out);
        let ok = match d.outcome {
            Outcome::Stat(stat) => {
                let head = &io.buf(rx)[..stat.head_len as usize];
                stat.size == size as u64 && header_in(head, meta_name) == Some(b"longshore check")
            }
            _ => false,
        };
        step(3, ok, d);

        // One level below the base: one key and one common prefix.
        s3.list_with(&mut io, 4, &base, None, 100, Some("/"), rx)
            .unwrap();
        let d = wait(&mut io, &mut s3, &mut out);
        let ok = match d.outcome {
            Outcome::Listed { len } => {
                Listing::parse(&io.buf(rx)[..len as usize]).is_ok_and(|listing| {
                    let mut keys = listing.entries();
                    let mut prefixes = listing.common_prefixes();
                    let key_ok = matches!(keys.next(), Some(Ok(e)) if e.key_raw == a.as_bytes());
                    let prefix_ok = matches!(prefixes.next(), Some(Ok(p))
                        if p.strip_prefix(base.as_bytes()) == Some(b"sub/"));
                    key_ok && prefix_ok && keys.next().is_none() && prefixes.next().is_none()
                })
            }
            _ => false,
        };
        step(4, ok, d);

        if gcs {
            // No batch erase on this store: one request each.
            for (k, key) in [(5, &a), (6, &b)] {
                s3.erase(&mut io, k as u64, key).unwrap();
                let d = wait(&mut io, &mut s3, &mut out);
                step(k, d.outcome == Outcome::Erased, d);
            }
        } else {
            let len = write_manifest(io.buf(tx), &[&a, &b, "selftest/never-there"])
                .unwrap_or_else(|| die("manifest does not fit")) as u32;
            s3.erase_many(&mut io, 5, tx, len, rx).unwrap();
            let d = wait(&mut io, &mut s3, &mut out);
            let ok = match d.outcome {
                Outcome::ErasedMany { len } => Failures::parse(&io.buf(rx)[..len as usize])
                    .is_ok_and(|mut failures| failures.next().is_none()),
                _ => false,
            };
            step(5, ok, d);
        }

        s3.stat(&mut io, 7, &a, None).unwrap();
        let d = wait(&mut io, &mut s3, &mut out);
        step(7, d.outcome == Outcome::NotFound, d);

        s3.presign(&mut io, &mut url, Access::Fetch, &a, 60).ok()
    });
    let ms = (io.now_ns() - t0) / MS;

    let names = [
        "store with options",
        "stat",
        "copy",
        "stat of the copy",
        "list with delimiter",
        if gcs { "erase" } else { "erase many" },
        "erase (second key)",
        "stat after erase",
    ];
    let mut ok = true;
    for (name, (step_ok, done)) in names.iter().zip(results.iter()) {
        match done {
            Some(d) => {
                ok &= step_ok;
                let text = format!("{:?} (attempts {})", d.outcome, d.attempts);
                lines.push((name, *step_ok, text));
            }
            None => lines.push((name, true, "skipped".to_string())),
        }
    }
    for (name, step_ok, text) in &lines {
        println!(
            "{name:22} -> {} {text}",
            if *step_ok { "ok  " } else { "FAIL" }
        );
    }
    // A presigned URL is a credential for as long as it lives: it is only
    // printed when asked for, to try it with another HTTP client.
    match url_len {
        Some(n) if std::env::var("S3_PRINT_PRESIGNED").is_ok_and(|v| v == "1") => {
            println!("presigned_url={}", String::from_utf8_lossy(&url[..n]))
        }
        Some(n) => println!("presigned_url_bytes={n} (set S3_PRINT_PRESIGNED=1 to print it)"),
        None => println!("presigned_url=unsupported with these credentials"),
    }
    println!("elapsed_ms={ms} allocation_sealed=true");
    println!("{}", if ok { "objects ok" } else { "objects FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
