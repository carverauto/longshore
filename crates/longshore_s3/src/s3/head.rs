// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The request line and headers of one attempt, in the store vendor's form:
//! the vendor's names for optional headers, then a bearer token for Google
//! Cloud Storage or a SigV4 signature for S3. Everything is written into
//! stack buffers the attempt owns.

use super::exact::{self, Encryption};
use super::*;
use crate::text::written;

/// Headers handed to the HTTP client.
pub(super) type Headers<'h> = [(&'h str, &'h [u8]); sigv4::HEADERS_MAX];

/// What one attempt's request line and headers are made from.
pub(super) struct Head<'a> {
    pub(super) s: &'a Slot,
    pub(super) method: &'static str,
    pub(super) payload: &'static str,
    pub(super) path: &'a str,
    pub(super) target: &'a str,
    pub(super) multipart_query: Option<&'a str>,
    pub(super) copy_source: Option<&'a str>,
    /// "bytes=a-b", or empty.
    pub(super) range: &'a str,
    pub(super) options: Named<'a>,
}

/// Stack space for the request target and the range of one attempt.
pub(super) struct Text {
    target: [u8; PATH_MAX + 1 + QUERY_MAX],
    range: [u8; 48],
}

/// Stack space for header values one attempt writes.
pub(super) struct Bufs {
    auth: [u8; 7 + TOKEN_MAX],
    goog_hash: [u8; 15],
    crc: [u8; 12],
    sha: [u8; 44],
}

/// Optional headers under the names this vendor gives them.
pub(super) struct Named<'a> {
    names: [u8; OPTIONS_MAX * NAME_MAX],
    ends: [usize; OPTIONS_MAX],
    values: [&'a str; OPTIONS_MAX],
    count: usize,
}

impl Text {
    pub(super) const fn new() -> Text {
        Text {
            target: [0; PATH_MAX + 1 + QUERY_MAX],
            range: [0; 48],
        }
    }
}

impl Bufs {
    pub(super) const fn new() -> Bufs {
        Bufs {
            auth: [0; 7 + TOKEN_MAX],
            goog_hash: [0; 15],
            crc: [0; 12],
            sha: [0; 44],
        }
    }
}

impl<'a> Head<'a> {
    /// The parts of slot `s`'s request, with text written into `text`.
    pub(super) fn of(s: &'a Slot, vendor: Vendor, text: &'a mut Text) -> Head<'a> {
        let path = written(&s.path[..s.path_len]);
        // A multipart request's target is the key and its query.
        let multipart_query = core::str::from_utf8(&s.query[..s.query_len])
            .ok()
            .filter(|q| !q.is_empty() && !s.comparing && s.kind != Kind::Copy);
        let copy_source = core::str::from_utf8(&s.query[..s.query_len])
            .ok()
            .filter(|_| s.kind == Kind::Copy);
        let (method, payload) = method_and_payload(s);
        let Text { target, range } = text;
        Head {
            s,
            method,
            payload,
            path,
            target: write_target(target, path, multipart_query),
            multipart_query,
            copy_source,
            range: write_range(range, s.kind),
            options: Named::of(s, vendor),
        }
    }
}

impl<'a> Named<'a> {
    fn of(s: &'a Slot, vendor: Vendor) -> Named<'a> {
        let vendor_prefix = match vendor {
            Vendor::S3 => "x-amz-",
            Vendor::Gcs => "x-goog-",
        };
        let mut named = Named {
            names: [0; OPTIONS_MAX * NAME_MAX],
            ends: [0; OPTIONS_MAX],
            values: [""; OPTIONS_MAX],
            count: 0,
        };
        if matches!(s.kind, Kind::Store { .. } | Kind::Begin { .. }) && !s.comparing {
            let mut at = 0;
            for (prefix, name, value) in s.options.headers(vendor_prefix) {
                for part in [prefix, name] {
                    named.names[at..at + part.len()].copy_from_slice(part.as_bytes());
                    at += part.len();
                }
                named.ends[named.count] = at;
                named.values[named.count] = value;
                named.count += 1;
            }
        }
        named
    }

    /// Option `k`: its name and value.
    fn get(&self, k: usize) -> (&str, &'a str) {
        let start = if k == 0 { 0 } else { self.ends[k - 1] };
        (written(&self.names[start..self.ends[k]]), self.values[k])
    }
}

/// The method, and the payload hash S3 signs, for slot `s`.
fn method_and_payload(s: &Slot) -> (&'static str, &'static str) {
    match s.kind {
        Kind::Store { .. } if s.comparing => ("HEAD", EMPTY_SHA256),
        Kind::Store { .. } => ("PUT", UNSIGNED_PAYLOAD),
        Kind::Fetch { .. } | Kind::List => ("GET", EMPTY_SHA256),
        Kind::Erase | Kind::Abort => ("DELETE", EMPTY_SHA256),
        Kind::Begin { .. } => ("POST", EMPTY_SHA256),
        Kind::Part => ("PUT", UNSIGNED_PAYLOAD),
        // The comparison after a lost completion asks about the object itself.
        Kind::Complete if s.comparing => ("HEAD", EMPTY_SHA256),
        Kind::Complete => ("POST", UNSIGNED_PAYLOAD),
        Kind::Stat => ("HEAD", EMPTY_SHA256),
        Kind::Copy => ("PUT", EMPTY_SHA256),
        Kind::EraseMany => ("POST", UNSIGNED_PAYLOAD),
    }
}

/// `path?query` written into `buf`, or `path` alone without a query.
fn write_target<'b>(buf: &'b mut [u8], path: &'b str, query: Option<&str>) -> &'b str {
    let Some(q) = query else {
        return path;
    };
    let n = path.len() + 1 + q.len();
    buf[..path.len()].copy_from_slice(path.as_bytes());
    buf[path.len()] = b'?';
    buf[path.len() + 1..n].copy_from_slice(q.as_bytes());
    written(&buf[..n])
}

/// "bytes=a-b" for a ranged fetch, written into `buf` without allocating;
/// empty for anything else.
fn write_range(buf: &mut [u8; 48], kind: Kind) -> &str {
    let Kind::Fetch {
        range: Some((a, b)),
    } = kind
    else {
        return "";
    };
    let mut len = 0;
    let mut put = |bytes: &[u8]| {
        buf[len..len + bytes.len()].copy_from_slice(bytes);
        len += bytes.len();
    };
    let digits = |mut v: u64, out: &mut [u8; 20]| -> usize {
        let mut k = 20;
        loop {
            k -= 1;
            out[k] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                return k;
            }
        }
    };
    let mut d = [0u8; 20];
    put(b"bytes=");
    let a_start = digits(a, &mut d);
    put(&d[a_start..]);
    put(b"-");
    let b_start = digits(b, &mut d);
    put(&d[b_start..]);
    written(&buf[..len])
}

/// Google Cloud Storage: no signature. The token is the whole proof, so
/// nothing depends on the other headers or on the clock.
pub(super) fn gcs_headers<'h>(
    head: &'h Head<'h>,
    token: &str,
    bufs: &'h mut Bufs,
    headers: &mut Headers<'h>,
) -> usize {
    let s = head.s;
    let Bufs {
        auth, goog_hash, ..
    } = bufs;
    let mut k = 0;
    if let (true, Some(crc), false) = (s.kind.stores_bytes(), s.crc, s.comparing) {
        *goog_hash = *b"crc32c=AAAAAAAA";
        goog_hash[7..].copy_from_slice(&longshore_crc::crc32c_header_value(crc as u32));
        headers[k] = ("x-goog-hash", &goog_hash[..]);
        k += 1;
    }
    if !head.range.is_empty() {
        headers[k] = ("range", head.range.as_bytes());
        k += 1;
    }
    if s.kind == (Kind::Store { no_clobber: true }) && !s.comparing {
        headers[k] = ("x-goog-if-generation-match", b"0");
        k += 1;
    }
    if let Some(source) = head.copy_source {
        headers[k] = ("x-goog-copy-source", source.as_bytes());
        k += 1;
    }
    for o in 0..head.options.count {
        let (name, value) = head.options.get(o);
        headers[k] = (name, value.as_bytes());
        k += 1;
    }
    let token = token.as_bytes();
    auth[..7].copy_from_slice(b"Bearer ");
    auth[7..7 + token.len()].copy_from_slice(token);
    headers[k] = ("authorization", &auth[..7 + token.len()]);
    k + 1
}

/// S3: sign the headers that are both sent and signed, then send them all
/// (but Host, which the HTTP client adds) with the Authorization header.
/// Records what was signed for an operation that reports it.
pub(super) fn s3_headers<'h>(
    head: &'h Head<'h>,
    cfg: &'h Config,
    creds: &'h Credentials,
    date: &'h [u8; 16],
    exact: &mut Option<exact::Exact>,
    bufs: &'h mut Bufs,
    headers: &mut Headers<'h>,
) -> usize {
    let Bufs { auth, crc, sha, .. } = bufs;
    let mut signed: [(&str, &str); sigv4::HEADERS_MAX] = [("", ""); sigv4::HEADERS_MAX];
    signed[0] = ("host", &cfg.host);
    signed[1] = ("x-amz-content-sha256", head.payload);
    signed[2] = ("x-amz-date", written(date));
    let checksum = Checksum {
        crc: crc_text(head.s.crc, crc),
        sha: sha_text(head.s.sha256, sha),
    };
    let m = 3 + s3_signed(head, creds, checksum, &mut signed[3..]);
    let scope = sigv4::Scope {
        access_key: creds.access_key(),
        secret_key: creds.secret_key(),
        region: &cfg.region,
        service: "s3",
        amz_date: date,
    };
    // A listing's target carries a query; it is signed as sorted pairs.
    let (uri, query) = match head.path.split_once('?') {
        Some((uri, query)) if matches!(head.s.kind, Kind::List | Kind::EraseMany) => (uri, query),
        _ => (head.path, head.multipart_query.unwrap_or("")),
    };
    let mut pairs = [("", ""); 5];
    let mut pair_count = 0;
    for kv in query.split('&').filter(|kv| !kv.is_empty()) {
        pairs[pair_count] = kv.split_once('=').unwrap_or((kv, ""));
        pair_count += 1;
    }
    let sreq = sigv4::Request {
        method: head.method,
        canonical_uri: uri,
        query: &pairs[..pair_count],
        headers: &signed[..m],
        payload_hash: head.payload,
    };
    #[expect(
        clippy::expect_used,
        reason = "sixteen headers at most are signed, and the Authorization buffer fits them all"
    )]
    let auth_len = authorization(&sreq, &scope, auth)
        .expect("at most sixteen headers, and the Authorization buffer always fits");
    if let Some(exact) = exact {
        exact.signing = Some(exact::describe(head.method, date, &sreq));
    }

    // Host is added by the HTTP client; send the rest plus Authorization.
    for (k, (name, value)) in signed[1..m].iter().enumerate() {
        headers[k] = (name, value.as_bytes());
    }
    headers[m - 1] = ("authorization", &auth[..auth_len]);
    m
}

/// The body's checksum headers, as text.
#[derive(Clone, Copy)]
struct Checksum<'h> {
    crc: Option<&'h str>,
    sha: Option<&'h str>,
}

fn crc_text(crc: Option<u64>, buf: &mut [u8; 12]) -> Option<&str> {
    *buf = longshore_crc::crc64_header_value(crc?);
    Some(written(buf))
}

fn sha_text(sha256: Option<[u8; 32]>, buf: &mut [u8; 44]) -> Option<&str> {
    let _: usize = longshore_crc::base64(&sha256?, buf);
    Some(written(buf))
}

/// Fill `signed` with the optional headers S3 signs, after host, content
/// hash and date; returns how many.
fn s3_signed<'h>(
    head: &'h Head<'h>,
    creds: &'h Credentials,
    checksum: Checksum<'h>,
    signed: &mut [(&'h str, &'h str)],
) -> usize {
    let s = head.s;
    let mut m = 0;
    let mut add = |header: (&'h str, &'h str)| {
        signed[m] = header;
        m += 1;
    };
    // A part carries its own checksum; a completion carries the whole object's.
    let checksummed = s.kind.stores_bytes() || matches!(s.kind, Kind::Complete | Kind::EraseMany);
    if let (true, Some(v), false) = (checksummed, checksum.crc, s.comparing) {
        add((CHECKSUM_HEADER, v));
    } else if let (Kind::Store { .. }, Some(v), false) = (s.kind, checksum.sha, s.comparing) {
        add(("x-amz-checksum-sha256", v));
    }
    let asked = s.exact.as_ref();
    if let Some(tag) = asked.and_then(|e| e.if_match.as_ref()) {
        add(("if-match", written(tag.as_bytes())));
    }
    if asked.and_then(|e| e.encryption) == Some(Encryption::Aes256) {
        add(("x-amz-server-side-encryption", "AES256"));
    }
    // Whole-object checksums have to be asked for when the upload begins.
    if s.kind == (Kind::Begin { checksums: true }) {
        add(("x-amz-checksum-algorithm", "CRC64NVME"));
        add(("x-amz-checksum-type", "FULL_OBJECT"));
    }
    // Ask the store to return the object's checksum with a whole-object
    // fetch, and when comparing a stored object with ours.
    if matches!(s.kind, Kind::Fetch { range: None } | Kind::Stat) || s.comparing {
        add(("x-amz-checksum-mode", "ENABLED"));
    }
    if !head.range.is_empty() {
        add(("range", head.range));
    }
    if s.kind == (Kind::Store { no_clobber: true }) && !s.comparing {
        add(("if-none-match", "*"));
    }
    if let Some(source) = head.copy_source {
        add(("x-amz-copy-source", source));
    }
    for o in 0..head.options.count {
        add(head.options.get(o));
    }
    // Temporary keys are only valid together with their session
    // token, which is sent and signed like any other header.
    if !creds.session_token().is_empty() {
        add(("x-amz-security-token", creds.session_token()));
    }
    m
}
