// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `put`, `head` and `get` against a real store: what it honours, what it
//! returns, and whether a write reads back as the object the store named.
//!
//!   exact_s3
//!
//! The store comes from the same environment variables as `s3_uring`:
//! S3_ENDPOINT, S3_REGION, S3_BUCKET, S3_ACCESS_KEY, S3_SECRET_KEY,
//! S3_SESSION_TOKEN, S3_TLS and S3_PATH_STYLE.
//!
//! It writes four small objects under a prefix of its own, each only if
//! absent and each one request: with encryption and a SHA-256, with the
//! SHA-256 alone, with encryption and a CRC-64, and with the CRC-64 alone.
//! It prints what the store answered to each, since stores differ in which
//! of these they accept and which they confirm. Then, with the first that
//! was stored: a second write that must be refused, reads bound to the
//! `ETag` and version the store returned, reads bound to another `ETag`
//! that must be refused, a body that does not match its SHA-256, and a
//! deadline that has passed. It erases what it wrote.
//!
//! It exits with a failure only if bytes read are not the bytes written,
//! or the client reports something that cannot be: what a store declines
//! to do is printed, not judged.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("exact_s3 needs Linux");
}

#[cfg(target_os = "linux")]
use linux::main;

#[cfg(target_os = "linux")]
mod linux {
    use longshore::client::{Budget, Buffer, Builder, Client, Error};
    use longshore::crypto::sha256;
    use longshore::store::creds::Credentials;
    use longshore::store::report::{Answer, Encryption, End, Expect, Put, Report};
    use longshore::store::s3::Checksum;
    use std::time::{Duration, Instant};

    const LEN: usize = 300_000;

    /// Print `exact_s3: <msg>` and exit with status 2.
    fn die(msg: &str) -> ! {
        eprintln!("exact_s3: {msg}");
        std::process::exit(2);
    }

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| die(&format!("{name} is not set")))
    }

    /// The client the store's environment variables describe.
    fn build() -> Client {
        let tls = std::env::var("S3_TLS").is_ok_and(|v| v == "1");
        let token = std::env::var("S3_SESSION_TOKEN").ok();
        let credentials = match token.filter(|t| !t.is_empty()) {
            Some(token) => Credentials::with_session(
                &env("S3_ACCESS_KEY"),
                &env("S3_SECRET_KEY"),
                token.trim(),
            ),
            None => Credentials::new(&env("S3_ACCESS_KEY"), &env("S3_SECRET_KEY")),
        }
        .unwrap_or_else(|| die("the credentials are empty, too long or not printable ASCII"));
        let path_style = std::env::var("S3_PATH_STYLE").is_ok_and(|v| v == "1");
        let host = if path_style {
            env("S3_ENDPOINT")
        } else {
            format!("{}.{}", env("S3_BUCKET"), env("S3_ENDPOINT"))
        };
        let mut b = Builder::new(&host, &env("S3_REGION"), credentials);
        b.bucket_in_path = path_style.then(|| env("S3_BUCKET"));
        (b.tls, b.port) = (tls, if tls { 443 } else { 80 });
        (b.connections, b.buffers, b.operations) = (2, 2, 2);
        b.buffer_size = 1 << 20;
        b.build().unwrap_or_else(|e| die(&e.to_string()))
    }

    /// The answer a request ended with, if it was answered.
    fn answer(report: &Result<Report, Error>) -> Option<Answer> {
        match report.as_ref().map(|r| r.end) {
            Ok(End::Answered(a)) => Some(a),
            _ => None,
        }
    }

    fn once() -> Budget {
        Budget::once(Instant::now() + Duration::from_secs(30))
    }

    /// The first object stored: its key, how it was written, and what binds
    /// a read to it.
    type First = (String, Put, Expect);

    /// The client, the bytes every object holds, and where they go.
    struct Run {
        client: Client,
        bytes: Vec<u8>,
        digest: [u8; 32],
        prefix: String,
    }

    impl Run {
        /// One line per answer: everything the store said, and nothing it did not.
        fn show(&self, what: &str, report: &Result<Report, Error>) {
            let digest = self.digest;
            let text = |raw: &[u8]| String::from_utf8_lossy(raw).into_owned();
            match report {
                Err(e) => println!("{what}: nothing sent, {e:?}"),
                Ok(r) => match r.end {
                    End::NotSent(cause) => println!("{what}: not sent, {cause:?}"),
                    End::Unknown(cause) => {
                        println!("{what}: UNKNOWN outcome, {cause:?}, sent {}", r.sent)
                    }
                    End::Answered(a) => println!(
                        "{what}: status {} etag {} version {} encryption {:?} sha256 {} crc64 {} \
                         length {:?} range {:?} body {} request-id {} malformed {} sent {} unanswered {}",
                        a.status,
                        a.etag.map_or("-".into(), |v| text(v.as_bytes())),
                        a.version.map_or("-".into(), |v| text(v.as_bytes())),
                        a.encryption,
                        match a.sha256 {
                            Some(d) if d == digest => "matches",
                            Some(_) => "DIFFERS",
                            None => "-",
                        },
                        a.crc64.map_or("-".into(), |v| text(&v)),
                        a.content_length,
                        a.content_range.map(|range| (range.first, range.last, range.size)),
                        a.body_len,
                        a.request_id.map_or("-", |_| "present"),
                        a.malformed,
                        r.sent,
                        r.unanswered,
                    ),
                },
            }
        }

        /// Write the object four ways, each only if absent. Returns the keys
        /// that may hold something and the first that was stored.
        fn write_variants(&self) -> (Buffer, Vec<String>, Option<First>) {
            let (client, digest, bytes, prefix) =
                (&self.client, self.digest, &self.bytes, &self.prefix);
            let variants = [
                (
                    "encrypted-sha256",
                    Some(Encryption::Aes256),
                    Checksum::Sha256(digest),
                ),
                ("sha256", None, Checksum::Sha256(digest)),
                (
                    "encrypted-crc64",
                    Some(Encryption::Aes256),
                    Checksum::Compute,
                ),
                ("crc64", None, Checksum::Compute),
            ];
            let mut written = Vec::new();
            let mut first = None;
            let mut body = client.buffer_blocking();
            for (name, encryption, checksum) in variants {
                body.as_mut_slice()[..LEN].copy_from_slice(bytes);
                let key = format!("{prefix}/{name}");
                let options = Put {
                    no_clobber: true,
                    encryption,
                    checksum,
                };
                let put;
                (put, body) = client.put(&key, body, LEN, &options, once()).wait();
                self.show(&format!("put {name}"), &put);
                let Some(a) = answer(&put) else {
                    // An unknown outcome may have stored it: erase it all the same.
                    written.push(key);
                    continue;
                };
                if a.status == 200 {
                    written.push(key.clone());
                    let expect = Expect {
                        etag: a.etag,
                        // A bucket without versions answers nothing, or "null".
                        version: a.version.filter(|v| v.as_bytes() != b"null"),
                    };
                    let _: &mut (String, Put, Expect) = first.get_or_insert((key, options, expect));
                }
            }
            (body, written, first)
        }

        /// Write `first` again, then read it with and against what binds it.
        /// Returns whether the bytes read back were the bytes written.
        fn read_back(&self, first: First, mut body: Buffer) -> (Buffer, bool) {
            let (client, bytes) = (&self.client, &self.bytes);
            let (key, options, expect) = first;
            println!("reading back {key}");
            let again;
            (again, body) = client.put(&key, body, LEN, &options, once()).wait();
            self.show("put again, must be refused", &again);
            let other = Expect {
                etag: longshore::store::multipart::Fixed::new(
                    b"\"00000000000000000000000000000000\"",
                ),
                version: expect.version,
            };
            self.show("head", &client.head(&key, &expect, once()).wait());
            self.show(
                "head, another etag",
                &client.head(&key, &other, once()).wait(),
            );
            let whole;
            (whole, body) = client.get(&key, None, &expect, body, once()).wait();
            self.show("get", &whole);
            let whole_ok = answer(&whole).is_some_and(|a| {
                a.status == 200 && body.as_slice()[..a.body_len as usize] == bytes[..]
            });
            let part;
            (part, body) = client
                .get(&key, Some((1000, 1999)), &expect, body, once())
                .wait();
            self.show("get bytes 1000-1999", &part);
            let part_ok = answer(&part).is_some_and(|a| {
                let got = &body.as_slice()[..a.body_len as usize];
                match a.status {
                    206 => got == &bytes[1000..2000],
                    200 => got == &bytes[..],
                    _ => false,
                }
            });
            println!("bytes read back: whole {whole_ok}, part {part_ok}");
            let refused;
            (refused, body) = client.get(&key, Some((0, 9)), &other, body, once()).wait();
            self.show("get, another etag", &refused);
            (body, whole_ok && part_ok)
        }

        /// A body sent with the SHA-256 of other bytes.
        fn put_wrong_sha256(&self, mut body: Buffer, written: &mut Vec<String>) -> Buffer {
            let (client, prefix, bytes) = (&self.client, &self.prefix, &self.bytes);
            let wrong = Put {
                no_clobber: true,
                encryption: None,
                checksum: Checksum::Sha256(sha256(b"other bytes")),
            };
            let wrong_key = format!("{prefix}/wrong-sha256");
            body.as_mut_slice()[..LEN].copy_from_slice(bytes);
            let damaged;
            (damaged, body) = client.put(&wrong_key, body, LEN, &wrong, once()).wait();
            self.show("put with a SHA-256 of other bytes", &damaged);
            written.push(wrong_key);
            body
        }

        /// A put whose deadline has passed must not be sent. Returns whether
        /// it was.
        fn put_late(&self, mut body: Buffer) -> bool {
            let (client, prefix, bytes) = (&self.client, &self.prefix, &self.bytes);
            let past = Budget::once(Instant::now() - Duration::from_millis(1));
            let late_key = format!("{prefix}/late");
            body.as_mut_slice()[..LEN].copy_from_slice(bytes);
            let (late, _body) = client
                .put(&late_key, body, LEN, &Put::default(), past)
                .wait();
            self.show("put after its deadline", &late);
            !matches!(late, Ok(r) if matches!(r.end, End::NotSent(_)) && r.request.is_none())
        }
    }

    pub fn main() {
        let client = build();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let prefix = format!("longshore-exact/{stamp}-{}", std::process::id());
        let bytes: Vec<u8> = (0..LEN as u32)
            .map(|i| i.wrapping_mul(2246822519).to_le_bytes()[2])
            .collect();
        let digest = sha256(&bytes);
        let run = Run {
            client,
            bytes,
            digest,
            prefix,
        };
        let (body, mut written, first) = run.write_variants();
        let mut failed = false;
        let body = if let Some(first) = first {
            let (body, ok) = run.read_back(first, body);
            failed |= !ok;
            body
        } else {
            println!("no variant was stored: nothing to read back");
            failed = true;
            body
        };
        let body = run.put_wrong_sha256(body, &mut written);
        failed |= run.put_late(body);

        for key in &written {
            if let Err(e) = run.client.erase(key).wait() {
                println!("could not erase {key}: {e:?}");
            }
        }
        println!("erased {} keys under {}", written.len(), run.prefix);
        if failed {
            die("FAILED: see above");
        }
        println!("ok exact_s3");
    }
}
