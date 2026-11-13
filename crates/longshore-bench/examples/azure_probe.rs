// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Checks the Azure Shared Key signer and the Blob request forms against a
//! Blob endpoint, by default the Azurite emulator with its public
//! development account. Plain blocking sockets: this probes the protocol,
//! not the I/O path.
//!
//!   cargo run -p longshore-bench --example azure_probe [HOST:PORT]

use longshore::store::sharedkey::{authorization, decode_key, Request, KEY_MAX};
use longshore::store::time::http_date;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

const ACCOUNT: &str = "devstoreaccount1";
/// The emulator's development key: public, and protects nothing.
const KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
const VERSION: &str = "2021-08-06";

struct Reply {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|l| {
            l.split_once(':')
                .filter(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.trim())
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn call(
    addr: &str,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    extra_ms: &[(&str, &str)],
    if_none_match: &str,
    range: &str,
    body: &[u8],
) -> Reply {
    let mut key = [0u8; KEY_MAX];
    let k = decode_key(KEY, &mut key).expect("development key");
    let date = http_date(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    let date = std::str::from_utf8(&date).unwrap();
    let mut ms: Vec<(&str, &str)> = vec![("x-ms-date", date), ("x-ms-version", VERSION)];
    ms.extend_from_slice(extra_ms);
    let content_type = if body.is_empty() {
        ""
    } else {
        "application/octet-stream"
    };
    let req = Request {
        method,
        content_length: body.len() as u64,
        content_type,
        if_none_match,
        range,
        ms_headers: &ms,
        account: ACCOUNT,
        path,
        query,
    };
    let mut auth = [0u8; 160];
    let n = authorization(&req, &key[..k], &mut auth).expect("signable");

    let mut target = path.to_string();
    for (i, (name, value)) in query.iter().enumerate() {
        target += &format!("{}{name}={value}", if i == 0 { '?' } else { '&' });
    }
    let mut head = format!("{method} {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\nAuthorization: {}\r\n", body.len(), std::str::from_utf8(&auth[..n]).unwrap());
    for (name, value) in &ms {
        head += &format!("{name}: {value}\r\n");
    }
    for (name, value) in [
        ("Content-Type", content_type),
        ("If-None-Match", if_none_match),
        ("Range", range),
    ] {
        if !value.is_empty() {
            head += &format!("{name}: {value}\r\n");
        }
    }
    head += "\r\n";
    let mut s = TcpStream::connect(addr).unwrap_or_else(|e| panic!("{addr}: {e}"));
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|v| v.parse().ok())
        .expect("a status");
    Reply {
        status,
        head,
        body: raw[split + 4..].to_vec(),
    }
}

fn show(what: &str, r: &Reply, want: u16) -> bool {
    let code = if r.status >= 400 {
        String::from_utf8_lossy(&r.body)
            .split("<Code>")
            .nth(1)
            .and_then(|s| s.split('<').next())
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };
    let ok = r.status == want;
    println!(
        "{:<44} -> {} {}{}",
        what,
        r.status,
        code,
        if ok { "" } else { "   UNEXPECTED" }
    );
    ok
}

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:10000".into());
    let addr = addr.as_str();
    let container = format!("/{ACCOUNT}/probe-{}", std::process::id());
    let blob = format!("{container}/dir/key%20one.bin");
    let body: Vec<u8> = (0..100_000u32)
        .map(|i| i.wrapping_mul(2654435761).to_le_bytes()[2])
        .collect();
    let crc = longshore::checksum::crc64_nvme(&body);
    let be = longshore::checksum::crc64_header_value(crc);
    let be = std::str::from_utf8(&be).unwrap();
    let le = longshore::checksum::crc64_azure_header_value(crc);
    let le = std::str::from_utf8(&le).unwrap();
    let block = ("x-ms-blob-type", "BlockBlob");
    let mut ok = true;

    ok &= show(
        "create container",
        &call(
            addr,
            "PUT",
            &container,
            &[("restype", "container")],
            &[],
            "",
            "",
            b"",
        ),
        201,
    );
    let r = call(addr, "PUT", &blob, &[], &[block], "*", "", &body);
    ok &= show("store if absent", &r, 201);
    let returned = r
        .header("x-ms-content-crc64")
        .unwrap_or("(none)")
        .to_string();
    println!("  store response x-ms-content-crc64: {returned}");
    println!(
        "  CRC-64/NVME of the body, most significant byte first: {be}{}",
        if returned == be { "   <- matches" } else { "" }
    );
    println!(
        "  CRC-64/NVME of the body, least significant byte first: {le}{}",
        if returned == le { "   <- matches" } else { "" }
    );
    // The reference documents 412 for an unmet If-None-Match on a write;
    // the emulator answers 409 BlobAlreadyExists. The client must take both.
    let r = call(addr, "PUT", &blob, &[], &[block], "*", "", &body);
    ok &= show("store if absent, again", &r, r.status) && matches!(r.status, 409 | 412);
    ok &= show(
        "store with checksum, least significant first",
        &call(
            addr,
            "PUT",
            &blob,
            &[],
            &[block, ("x-ms-content-crc64", le)],
            "",
            "",
            &body,
        ),
        201,
    );
    ok &= show(
        "store with checksum, most significant first",
        &call(
            addr,
            "PUT",
            &blob,
            &[],
            &[block, ("x-ms-content-crc64", be)],
            "",
            "",
            &body,
        ),
        400,
    );
    ok &= show(
        "store with a wrong checksum",
        &call(
            addr,
            "PUT",
            &blob,
            &[],
            &[block, ("x-ms-content-crc64", "AAAAAAAAAAA=")],
            "",
            "",
            &body,
        ),
        400,
    );
    let r = call(addr, "GET", &blob, &[], &[], "", "", b"");
    ok &= show("fetch", &r, 200) && r.body == body;
    let r = call(addr, "GET", &blob, &[], &[], "", "bytes=100-199", b"");
    ok &= show("fetch a range", &r, 206) && r.body == body[100..200];
    let r = call(addr, "HEAD", &blob, &[], &[], "", "", b"");
    ok &= show("properties", &r, 200);
    println!(
        "  properties x-ms-content-crc64: {}   content-md5: {}",
        r.header("x-ms-content-crc64").unwrap_or("(none)"),
        r.header("content-md5").unwrap_or("(none)")
    );
    let r = call(
        addr,
        "GET",
        &container,
        &[
            ("restype", "container"),
            ("comp", "list"),
            ("prefix", "dir/"),
        ],
        &[],
        "",
        "",
        b"",
    );
    ok &= show("list with a prefix", &r, 200);
    println!(
        "  listing: {}",
        String::from_utf8_lossy(&r.body)
            .replace(['\r', '\n'], "")
            .chars()
            .take(600)
            .collect::<String>()
    );
    ok &= show(
        "erase",
        &call(addr, "DELETE", &blob, &[], &[], "", "", b""),
        202,
    );
    ok &= show(
        "fetch after erase",
        &call(addr, "GET", &blob, &[], &[], "", "", b""),
        404,
    );
    ok &= show(
        "delete container",
        &call(
            addr,
            "DELETE",
            &container,
            &[("restype", "container")],
            &[],
            "",
            "",
            b"",
        ),
        202,
    );
    println!("{}", if ok { "probe ok" } else { "probe FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
