// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Correctness commands: the self-test, the wrong-checksum probe, and listing or erasing a prefix.

use super::*;

/// List everything under `prefix`, a page at a time, optionally erasing
/// each key. Erasing shrinks the listing, so a clean always re-lists from
/// the start; a plain list pages by the last key seen.
pub(super) fn sweep(prefix: &str, erase: bool) {
    use osc_store::list::{unescape, Listing};
    if erase && prefix.is_empty() {
        die("refusing to clean with an empty prefix");
    }
    const CONNS: u32 = 8;
    let (mut io, mut s3, first) = setup(CONNS, 1, 1 << 20, CONNS);
    let page = BufId(first);
    let mut out = Completions::new(64);
    let (mut count, mut bytes, mut failed) = (0u64, 0u64, 0u64);
    let mut after: Option<String> = None;
    loop {
        s3.list(&mut io, 0, prefix, after.as_deref(), 1000, page)
            .unwrap_or_else(|e| die(&format!("list: {e:?}")));
        let Outcome::Listed { len } = wait(&mut io, &mut s3, &mut out).outcome else {
            die("list failed")
        };
        let xml = io.buf(page)[..len as usize].to_vec();
        let listing = Listing::parse(&xml).unwrap_or_else(|e| die(&format!("listing: {e:?}")));
        let keys: Vec<(String, u64)> = listing
            .entries()
            .map(|e| {
                let e = e.unwrap_or_else(|e| die(&format!("listing entry: {e:?}")));
                let mut k = vec![0u8; e.key_raw.len()];
                let n = unescape(e.key_raw, &mut k).unwrap_or_else(|| die("bad key escape"));
                (String::from_utf8_lossy(&k[..n]).into_owned(), e.size)
            })
            .collect();
        if keys.is_empty() {
            break;
        }
        for chunk in keys.chunks(CONNS as usize) {
            for (i, (key, size)) in chunk.iter().enumerate() {
                count += 1;
                bytes += size;
                if erase {
                    s3.erase(&mut io, i as u64, key)
                        .unwrap_or_else(|e| die(&format!("erase: {e:?}")));
                } else {
                    println!("{size:>12}  {key}");
                }
            }
            if erase {
                for _ in chunk {
                    if wait(&mut io, &mut s3, &mut out).outcome != Outcome::Erased {
                        failed += 1;
                    }
                }
            }
        }
        if erase {
            // If nothing could be erased, stop instead of listing the same page forever.
            if failed >= count {
                break;
            }
        } else if listing.is_truncated {
            after = keys.last().map(|k| k.0.clone());
        } else {
            break;
        }
    }
    println!(
        "{} {count} objects, {:.1} MiB under {prefix:?}{}",
        if erase { "erased" } else { "listed" },
        bytes as f64 / (1 << 20) as f64,
        if failed > 0 {
            format!(" ({failed} FAILED)")
        } else {
            String::new()
        }
    );
    std::process::exit(if failed > 0 { 1 } else { 0 });
}

/// Store an object with a checksum that is deliberately wrong and report
/// what the store does. A store that enforces checksums refuses every
/// attempt, and the object must not exist afterwards.
pub(super) fn bad_checksum(size: u32) {
    let (mut io, mut s3, first) = setup(1, 2, size, 2);
    let (tx, rx) = (BufId(first), BufId(first + 1));
    io.buf(tx)[..size as usize].fill(0x5a);
    let key = format!(
        "selftest/badcrc-{}-{}.bin",
        std::process::id(),
        io.unix_secs()
    );
    let mut out = Completions::new(32);
    s3.store(&mut io, 0, &key, tx, size, Checksum::Provided(1))
        .unwrap();
    let stored = wait(&mut io, &mut s3, &mut out);
    s3.fetch(&mut io, 1, &key, None, rx).unwrap();
    let fetched = wait(&mut io, &mut s3, &mut out);
    s3.erase(&mut io, 2, &key).unwrap();
    wait(&mut io, &mut s3, &mut out);
    println!(
        "store with a wrong checksum -> {:?} (attempts {})",
        stored.outcome, stored.attempts
    );
    println!("fetch afterwards            -> {:?}", fetched.outcome);
    let enforced = stored.outcome != Outcome::Stored && fetched.outcome == Outcome::NotFound;
    println!("checksum_enforced={enforced}");
}

pub(super) fn selftest(size: u32) {
    let (mut io, mut s3, first) = setup(1, 2, size, 2);
    let (tx, rx) = (BufId(first), BufId(first + 1));
    for (i, b) in io.buf(tx)[..size as usize].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[2];
    }
    let key = format!("selftest/{}-{}.bin", std::process::id(), io.unix_secs());
    let mut out = Completions::new(32);
    let t0 = io.now_ns();

    // Sealed: the whole exchange, including DNS, signing and retries.
    let (steps, range_ok) = sealed(|| {
        let mut steps = [None; 6];
        let mut range_ok = false;
        s3.store_no_clobber(&mut io, 0, &key, tx, size, Checksum::Compute)
            .unwrap();
        steps[0] = Some(wait(&mut io, &mut s3, &mut out));
        s3.store_no_clobber(&mut io, 1, &key, tx, size, Checksum::Compute)
            .unwrap();
        steps[1] = Some(wait(&mut io, &mut s3, &mut out));
        s3.fetch(&mut io, 2, &key, None, rx).unwrap();
        steps[2] = Some(wait(&mut io, &mut s3, &mut out));
        if size >= 2000 {
            // Overwrites the start of rx; the full comparison below uses the tail.
            s3.fetch(&mut io, 3, &key, Some((1000, 1999)), rx).unwrap();
            steps[3] = Some(wait(&mut io, &mut s3, &mut out));
            let mut same = true;
            for i in 0..1000usize {
                same &=
                    io.buf(rx)[i] == ((1000 + i) as u32).wrapping_mul(2654435761).to_le_bytes()[2];
            }
            range_ok = same;
        }
        s3.erase(&mut io, 4, &key).unwrap();
        steps[4] = Some(wait(&mut io, &mut s3, &mut out));
        s3.fetch(&mut io, 5, &key, None, rx).unwrap();
        steps[5] = Some(wait(&mut io, &mut s3, &mut out));
        (steps, range_ok)
    });
    let ms = (io.now_ns() - t0) / MS;

    let names = [
        "store_no_clobber (new)",
        "store_no_clobber (again)",
        "fetch",
        "fetch range 1000-1999",
        "erase",
        "fetch after erase",
    ];
    for (n, s) in names.iter().zip(steps.iter()) {
        match s {
            Some(d) => println!("{n:26} -> {:?} (attempts {})", d.outcome, d.attempts),
            None => println!("{n:26} -> skipped"),
        }
    }
    // The ranged fetch overwrote rx[..1000]; compare the rest of the full fetch.
    let from = if size >= 2000 { 1000 } else { 0 };
    let sent = io.buf(tx)[from..size as usize].to_vec();
    let tail_ok = sent[..] == io.buf(rx)[from..size as usize];
    let o = |i: usize| steps[i].map(|d| d.outcome);
    let ok = o(0) == Some(Outcome::Stored)
        && matches!(o(1), Some(Outcome::AlreadyExists { .. }))
        && matches!(o(2), Some(Outcome::Fetched { len, .. }) if len == size)
        && (size < 2000 || (matches!(o(3), Some(Outcome::Fetched { len: 1000, .. })) && range_ok))
        && o(4) == Some(Outcome::Erased)
        && o(5) == Some(Outcome::NotFound)
        && tail_ok;
    println!("bytes_match={tail_ok} range_match={range_ok} connections={} elapsed_ms={ms} enters={} allocation_sealed=true", s3.connects(), io.enters());
    println!("{}", if ok { "selftest ok" } else { "selftest FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
