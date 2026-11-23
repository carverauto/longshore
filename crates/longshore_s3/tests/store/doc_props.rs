// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) for the small documents the store reads and
//! writes besides listings: multipart and batch-erase bodies, error
//! documents and response heads.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_s3::creds::Credentials;
use longshore_s3::erase::{self, Failure, Failures};
use longshore_s3::list::{unescape, ListError};
use longshore_s3::multipart::{self, Part, PartTag, PART_NUMBER_MAX, PART_TAG_MAX};
use longshore_s3::sharedkey;
use longshore_s3::stat::header_in;
use longshore_s3::time::parse_http_date;

/// `part` is a slice of `whole`, not a copy of some of it.
fn inside(part: &[u8], whole: &[u8]) -> bool {
    let (p, w) = (part.as_ptr_range(), whole.as_ptr_range());
    part.is_empty() || (w.start <= p.start && p.end <= w.end)
}

#[cfg(test)]
fn unescaped(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; raw.len()];
    let n = unescape(raw, &mut out).expect("references this crate wrote");
    out.truncate(n);
    out
}

/// The texts between each `open` and the `close` after it.
#[cfg(test)]
fn between<'a>(doc: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    doc.split(open)
        .skip(1)
        .map(|rest| rest.split_once(close).expect("an unclosed element").0)
        .collect()
}

/// Arbitrary bytes never panic a parser of bytes from the network or from
/// a file, and what a parser returns is part of what it was given.
#[hegel::test]
fn arbitrary_bytes_are_safe_to_parse(tc: TestCase) {
    const STARTS: [&[u8]; 6] = [
        b"",
        b"<InitiateMultipartUploadResult><UploadId>",
        b"<Error><Code>",
        b"<DeleteResult><Error><Key>",
        b"HTTP/1.1 200 OK\r\nETag: ",
        b"[default]\naws_access_key_id = ",
    ];
    let start = STARTS[tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(STARTS.len() - 1),
    )];
    let data = [start, &tc.draw(gs::binary().max_size(300))].concat();

    if let Some(id) = multipart::upload_id_in(&data) {
        assert!(!id.is_empty() && inside(id, &data));
    }
    if let Some(code) = multipart::error_code_in(&data) {
        assert!(inside(code, &data));
    }
    if let Ok(failures) = Failures::parse(&data) {
        let all: Vec<_> = failures.collect();
        // An error ends the report: nothing is read past damage.
        let errors = all.iter().filter(|f| f.is_err()).count();
        assert!(errors <= 1 && all.iter().rev().skip(1).all(|f| f.is_ok()));
        for f in all.into_iter().flatten() {
            assert!(!f.key_raw().is_empty() && inside(f.key_raw(), &data));
            assert!(inside(f.code(), &data));
        }
    }
    for name in ["etag", "content-length", ""] {
        if let Some(value) = header_in(&data, name) {
            assert!(inside(value, &data));
        }
    }
    let _: Option<u64> = parse_http_date(&data);
    let _: Option<Credentials> = Credentials::parse(&data);
    let _: Option<Credentials> = Credentials::parse_bearer(&data);
    if let Ok(text) = core::str::from_utf8(&data) {
        let _: Option<usize> = sharedkey::decode_key(text, &mut [0u8; sharedkey::KEY_MAX]);
    }
}

/// The keys a batch erase names are the keys it was asked to name, whatever
/// characters they hold, and the document needs exactly the room it takes.
#[hegel::test]
fn an_erase_manifest_names_its_keys(tc: TestCase) {
    let keys: Vec<String> = (0..tc.draw(gs::integers::<usize>().min_value(1).max_value(8)))
        .map(|_| tc.draw(gs::text().min_size(1).max_size(30)))
        .collect();
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let mut out = vec![0u8; 8 * 30 * 6 * 4 + 256];
    let n = erase::write_manifest(&mut out, &refs).expect("the buffer is large enough");
    let doc = core::str::from_utf8(&out[..n]).expect("keys are text");
    assert!(doc.starts_with("<Delete><Quiet>true</Quiet><Object>") && doc.ends_with("</Delete>"));
    let named: Vec<Vec<u8>> = between(doc, "<Object><Key>", "</Key></Object>")
        .into_iter()
        .map(|k| {
            // Nothing a parser would read as markup, or drop, is left raw.
            assert!(!k.contains(['<', '>', '"', '\r']), "{k:?}");
            unescaped(k.as_bytes())
        })
        .collect();
    let asked: Vec<Vec<u8>> = keys.iter().map(|k| k.as_bytes().to_vec()).collect();
    assert_eq!(named, asked);

    let mut exact = vec![0u8; n];
    assert_eq!(erase::write_manifest(&mut exact, &refs), Some(n));
    assert_eq!(erase::write_manifest(&mut exact[..n - 1], &refs), None);
}

/// A completion document lists the parts it was given: numbers, tags and
/// checksums, in order.
#[hegel::test]
fn a_completion_manifest_lists_its_parts(tc: TestCase) {
    let mut number = 0;
    let mut parts = Vec::new();
    for _ in 0..tc.draw(gs::integers::<usize>().min_value(0).max_value(6)) {
        number += tc.draw(gs::integers::<u32>().min_value(1).max_value(1600));
        // A tag is whatever the store sent: not only quoted hex.
        let mut tag = tc.draw(gs::binary().min_size(1).max_size(PART_TAG_MAX));
        tag.retain(|b| b.is_ascii_graphic());
        if tag.is_empty() {
            tag.push(b'"');
        }
        let checksum = tc
            .draw(gs::booleans())
            .then(|| longshore_crc::crc64_header_value(tc.draw(gs::integers::<u64>())));
        parts.push(Part::new(number, PartTag::new(&tag).unwrap(), checksum));
    }
    assert!(number <= PART_NUMBER_MAX);
    let mut out = vec![0u8; 4096];
    let n = multipart::write_manifest(&mut out, &parts).expect("ascending parts in a large buffer");
    let doc = core::str::from_utf8(&out[..n]).unwrap();

    let listed = between(doc, "<Part>", "</Part>");
    assert_eq!(listed.len(), parts.len());
    for (text, part) in listed.iter().zip(&parts) {
        let numbers = between(text, "<PartNumber>", "</PartNumber>");
        assert_eq!(numbers, [part.number().to_string()]);
        let tags = between(text, "<ETag>", "</ETag>");
        assert_eq!(tags.len(), 1);
        assert!(!tags[0].contains(['<', '>', '"']));
        assert_eq!(unescaped(tags[0].as_bytes()), part.tag().as_bytes());
        let sums: Vec<&[u8]> = between(text, "<ChecksumCRC64NVME>", "</ChecksumCRC64NVME>")
            .into_iter()
            .map(str::as_bytes)
            .collect();
        let checksum = part.checksum();
        let expected: Vec<&[u8]> = checksum.iter().map(|c| &c[..]).collect();
        assert_eq!(sums, expected);
    }
    assert_eq!(multipart::write_manifest(&mut out[..n - 1], &parts), None);

    // Out of order, or twice the same number, is refused.
    if parts.len() >= 2 {
        let at = tc.draw(
            gs::integers::<usize>()
                .min_value(0)
                .max_value(parts.len() - 2),
        );
        let mut swapped = parts.clone();
        swapped.swap(at, at + 1);
        assert_eq!(multipart::write_manifest(&mut out, &swapped), None);
        let mut repeated = parts.clone();
        let next = repeated[at + 1];
        repeated[at + 1] = Part::new(repeated[at].number(), next.tag(), next.checksum());
        assert_eq!(multipart::write_manifest(&mut out, &repeated), None);
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The failures a batch erase reports are read back as they were written,
/// whatever surrounds them.
#[hegel::test]
fn a_report_of_failures_is_read_back(tc: TestCase) {
    const CODE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let failures: Vec<(String, String)> = (0..tc
        .draw(gs::integers::<usize>().min_value(0).max_value(6)))
        .map(|_| {
            let mut key = tc.draw(gs::text().min_size(1).max_size(30));
            key.retain(|c| !c.is_control());
            if key.is_empty() {
                key.push('k');
            }
            let code = tc
                .draw(gs::binary().min_size(1).max_size(12))
                .iter()
                .map(|b| CODE[*b as usize % CODE.len()] as char)
                .collect();
            (key, code)
        })
        .collect();
    let mut doc = String::from("<?xml version=\"1.0\"?>\n<DeleteResult xmlns=\"x\">");
    for (key, code) in &failures {
        if tc.draw(gs::booleans()) {
            doc += "<Deleted><Key>gone</Key></Deleted>";
        }
        if tc.draw(gs::booleans()) {
            doc += &format!(
                "<Error><Key>{}</Key><Code>{code}</Code></Error>",
                escape(key)
            );
        } else {
            doc += &format!(
                "<Error><Code>{code}</Code><Message>no</Message><Key>{}</Key></Error>",
                escape(key)
            );
        }
    }
    doc += "</DeleteResult>";

    let read: Vec<Result<Failure, ListError>> = Failures::parse(doc.as_bytes()).unwrap().collect();
    let read: Vec<(String, String)> = read
        .into_iter()
        .map(|f| {
            let f = f.expect("a well-formed failure");
            (
                String::from_utf8(unescaped(f.key_raw())).unwrap(),
                String::from_utf8(f.code().to_vec()).unwrap(),
            )
        })
        .collect();
    assert_eq!(read, failures);

    // Cut anywhere, the document is refused or its damage is reported:
    // it never reads as a complete report of fewer failures.
    let cut = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(doc.len() - 1),
    );
    let complete = Failures::parse(&doc.as_bytes()[..cut])
        .map(|f| f.collect::<Result<Vec<_>, _>>().is_ok())
        .unwrap_or(false);
    assert!(!complete, "accepted a report cut at {cut} of {}", doc.len());
}

/// A header is found by its name in any case, and its value comes back
/// without the spaces around it.
#[hegel::test]
fn a_header_is_found_in_a_head_by_name(tc: TestCase) {
    const NAME: &[u8] = b"abcdefghijklmnopqrstuvwxyz-";
    const VALUE: &[u8] = b"abcXYZ019/+=;:,.\"'()*_~ ";
    let pick = |alphabet: &[u8], min: usize| -> String {
        tc.draw(gs::binary().min_size(min).max_size(12))
            .iter()
            .map(|b| alphabet[*b as usize % alphabet.len()] as char)
            .collect()
    };
    let headers: Vec<(String, String)> = (0..tc
        .draw(gs::integers::<usize>().min_value(0).max_value(6)))
        .map(|i| (format!("h{i}{}", pick(NAME, 0)), pick(VALUE, 0)))
        .collect();
    let mut head = String::from("HTTP/1.1 200 OK\r\n");
    for (name, value) in &headers {
        let pad = if tc.draw(gs::booleans()) { " " } else { "" };
        head += &format!("{name}:{pad}{value}{pad}\r\n");
    }
    head += "\r\n";
    for (name, value) in &headers {
        let asked = if tc.draw(gs::booleans()) {
            name.to_ascii_uppercase()
        } else {
            name.clone()
        };
        assert_eq!(
            header_in(head.as_bytes(), &asked),
            Some(value.trim().as_bytes()),
            "{asked:?} in {head:?}"
        );
    }
    assert_eq!(header_in(head.as_bytes(), "absent"), None);
    assert_eq!(header_in(head.as_bytes(), "HTTP/1.1 200 OK"), None);
}
