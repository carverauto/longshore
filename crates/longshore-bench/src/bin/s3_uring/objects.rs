// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The object-operations check: store with optional headers, stat, copy,
//! list one level with a delimiter, erase in a batch, and presign a URL.

use super::*;
use crate::die::OrDie;
use longshore::store::erase::{write_manifest, Failures};
use longshore::store::list::Listing;
use longshore::store::options::Options;
use longshore::store::presign::Access;
use longshore::store::stat::{header_in, Stat};

/// Whether a step passed, and what the store answered; `None` if skipped.
type Step = (bool, Option<Done>);

/// The stat in an outcome, if it is one.
fn stat_of(outcome: &Outcome) -> Option<Stat> {
    match outcome {
        Outcome::Stat(stat) => Some(*stat),
        Outcome::Stored
        | Outcome::AlreadyExists { .. }
        | Outcome::Fetched { .. }
        | Outcome::Listed { .. }
        | Outcome::NotFound
        | Outcome::Erased
        | Outcome::Begun { .. }
        | Outcome::PartStored { .. }
        | Outcome::Aborted
        | Outcome::Copied
        | Outcome::ErasedMany { .. }
        | Outcome::Report(_)
        | Outcome::Fatal(_) => None,
    }
}

/// The length of the listing in an outcome, if it is one.
fn listed_len(outcome: &Outcome) -> Option<u32> {
    match outcome {
        Outcome::Listed { len } => Some(*len),
        Outcome::Stored
        | Outcome::AlreadyExists { .. }
        | Outcome::Fetched { .. }
        | Outcome::NotFound
        | Outcome::Erased
        | Outcome::Begun { .. }
        | Outcome::PartStored { .. }
        | Outcome::Aborted
        | Outcome::Stat(_)
        | Outcome::Copied
        | Outcome::ErasedMany { .. }
        | Outcome::Report(_)
        | Outcome::Fatal(_) => None,
    }
}

/// The length of a batch erase's report in an outcome, if it is one.
fn erased_many_len(outcome: &Outcome) -> Option<u32> {
    match outcome {
        Outcome::ErasedMany { len } => Some(*len),
        Outcome::Stored
        | Outcome::AlreadyExists { .. }
        | Outcome::Fetched { .. }
        | Outcome::Listed { .. }
        | Outcome::NotFound
        | Outcome::Erased
        | Outcome::Begun { .. }
        | Outcome::PartStored { .. }
        | Outcome::Aborted
        | Outcome::Stat(_)
        | Outcome::Copied
        | Outcome::Report(_)
        | Outcome::Fatal(_) => None,
    }
}

/// The client and buffers every step uses: `tx` holds the object, `rx`
/// receives response heads, listings and reports.
struct Steps<'a> {
    io: &'a mut UringIo,
    s3: &'a mut S3,
    out: &'a mut Completions,
    tx: BufId,
    rx: BufId,
    size: u32,
}

impl Steps<'_> {
    fn wait(&mut self) -> Done {
        wait(self.io, self.s3, self.out)
    }

    fn store_with(&mut self, key: &str, options: &Options) -> Step {
        self.s3
            .store_with(
                self.io,
                0,
                key,
                self.tx,
                self.size,
                Checksum::Compute,
                options,
            )
            .or_die("store_with");
        let d = self.wait();
        (d.outcome() == Outcome::Stored, Some(d))
    }

    /// Stat `key` with its response head in `rx`, and judge the result with `check`.
    fn stat(&mut self, token: u64, key: &str, check: impl Fn(&Stat, &[u8]) -> bool) -> Step {
        self.s3
            .stat(self.io, token, key, Some(self.rx))
            .or_die("stat");
        let d = self.wait();
        let ok = stat_of(&d.outcome())
            .is_some_and(|stat| check(&stat, &self.io.buf(self.rx)[..stat.head_len() as usize]));
        (ok, Some(d))
    }

    fn copy(&mut self, from: &str, to: &str) -> Step {
        self.s3.copy(self.io, 2, from, to).or_die("copy");
        let d = self.wait();
        (d.outcome() == Outcome::Copied, Some(d))
    }

    /// One level below `base`: the key `a` and the common prefix `sub/`,
    /// listed in one page, then one entry at a time.
    fn list(&mut self, base: &str, a: &str) -> Step {
        self.s3
            .list_with(self.io, 4, base, None, 100, Some("/"), self.rx)
            .or_die("list_with");
        let d = self.wait();
        let ok = listed_len(&d.outcome()).is_some_and(|len| {
            Listing::parse(&self.io.buf(self.rx)[..len as usize]).is_ok_and(|listing| {
                let mut keys = listing.entries();
                let mut prefixes = listing.common_prefixes();
                let key_ok = matches!(keys.next(), Some(Ok(e)) if e.key_raw() == a.as_bytes());
                let prefix_ok = matches!(prefixes.next(), Some(Ok(p))
                    if p.strip_prefix(base.as_bytes()) == Some(b"sub/"));
                key_ok && prefix_ok && keys.next().is_none() && prefixes.next().is_none()
            })
        });
        let paged_ok = self.list_paged(base, a);
        (ok && paged_ok, Some(d))
    }

    /// The same level one entry at a time: the first page is cut short and
    /// returns a token, and the token brings the rest without repeating anything.
    fn list_paged(&mut self, base: &str, a: &str) -> bool {
        self.s3
            .list_with(self.io, 4, base, None, 1, Some("/"), self.rx)
            .or_die("list_with");
        let first_page = self.wait();
        let mut continuation = [0u8; 1100];
        let mut continuation_len = 0;
        let first_ok = listed_len(&first_page.outcome()).is_some_and(|len| {
            Listing::parse(&self.io.buf(self.rx)[..len as usize]).is_ok_and(|listing| {
                continuation_len = listing
                    .next_token_raw()
                    .and_then(|raw| longshore::store::list::unescape(raw, &mut continuation))
                    .unwrap_or(0);
                let one_key =
                    matches!(listing.entries().next(), Some(Ok(e)) if e.key_raw() == a.as_bytes());
                listing.is_truncated() && one_key && listing.common_prefixes().next().is_none()
            })
        });
        first_ok
            && core::str::from_utf8(&continuation[..continuation_len]).is_ok_and(|t| {
                !t.is_empty()
                    && self
                        .s3
                        .list_continue(self.io, 4, base, t, 1, Some("/"), self.rx)
                        .is_ok()
            })
            && listed_len(&self.wait().outcome()).is_some_and(|len| {
                Listing::parse(&self.io.buf(self.rx)[..len as usize]).is_ok_and(|listing| {
                    let mut prefixes = listing.common_prefixes();
                    let prefix_ok = matches!(prefixes.next(), Some(Ok(p))
                        if p.strip_prefix(base.as_bytes()) == Some(b"sub/"));
                    prefix_ok
                        && prefixes.next().is_none()
                        && listing.entries().next().is_none()
                        && !listing.is_truncated()
                })
            })
    }

    fn erase(&mut self, token: u64, key: &str) -> Step {
        self.s3.erase(self.io, token, key).or_die("erase");
        let d = self.wait();
        (d.outcome() == Outcome::Erased, Some(d))
    }

    /// Both keys and one that was never there, in one batch.
    fn erase_many(&mut self, a: &str, b: &str) -> Step {
        let manifest_len = write_manifest(self.io.buf(self.tx), &[a, b, "selftest/never-there"])
            .unwrap_or_else(|| die("manifest does not fit")) as u32;
        self.s3
            .erase_many(self.io, 5, self.tx, manifest_len, self.rx)
            .or_die("erase_many");
        let d = self.wait();
        let ok = erased_many_len(&d.outcome()).is_some_and(|len| {
            Failures::parse(&self.io.buf(self.rx)[..len as usize])
                .is_ok_and(|mut failures| failures.next().is_none())
        });
        (ok, Some(d))
    }

    fn stat_gone(&mut self, key: &str) -> Step {
        self.s3.stat(self.io, 7, key, None).or_die("stat");
        let d = self.wait();
        (d.outcome() == Outcome::NotFound, Some(d))
    }
}

/// Print each step and the presigned URL (or its length); true if every step passed.
fn report(gcs: bool, results: &[Step; 8], url: &[u8], url_len: Option<usize>, ms: u64) -> bool {
    let names = [
        "store with options",
        "stat",
        "copy",
        "stat of the copy",
        "list with delimiter, then paged by token",
        if gcs { "erase" } else { "erase many" },
        "erase (second key)",
        "stat after erase",
    ];
    let mut lines: Vec<(&str, bool, String)> = Vec::with_capacity(16);
    let mut ok = true;
    for (name, (step_ok, done)) in names.iter().zip(results.iter()) {
        match done {
            Some(d) => {
                ok &= step_ok;
                let text = format!("{:?} (attempts {})", d.outcome(), d.attempts());
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
    ok
}

pub(super) fn objects(size: u32) {
    let gcs = std::env::var("S3_VENDOR").is_ok_and(|v| v == "gcs");
    let (mut io, mut s3, first) = setup(1, 2, size, 2);
    let (tx, rx) = (BufId::new(first), BufId::new(first + 1));
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
    let mut results: [Step; 8] = [(false, None); 8];
    let url_len = sealed(|| {
        let mut c = Steps {
            io: &mut io,
            s3: &mut s3,
            out: &mut out,
            tx,
            rx,
            size,
        };
        results[0] = c.store_with(&a, &options);
        results[1] = c.stat(1, &a, |stat, head| {
            stat.size() == u64::from(size)
                && stat.tag().is_some()
                && header_in(head, "content-type") == Some(b"application/x-longshore-check")
                && header_in(head, meta_name) == Some(b"longshore check")
        });
        results[2] = c.copy(&a, &b);
        results[3] = c.stat(3, &b, |stat, head| {
            stat.size() == u64::from(size) && header_in(head, meta_name) == Some(b"longshore check")
        });
        results[4] = c.list(&base, &a);
        if gcs {
            // No batch erase on this store: one request each.
            results[5] = c.erase(5, &a);
            results[6] = c.erase(6, &b);
        } else {
            results[5] = c.erase_many(&a, &b);
        }
        results[7] = c.stat_gone(&a);
        c.s3.presign(c.io, &mut url, Access::Fetch, &a, 60).ok()
    });
    let ms = (io.now_ns() - t0) / MS;
    let ok = report(gcs, &results, &url, url_len, ms);
    println!("{}", if ok { "objects ok" } else { "objects FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
