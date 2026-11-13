// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests for the DNS codec (Hegel).

use hegel::generators as gs;
use hegel::TestCase;
use osc_dns::codec::{encode_query, parse_response, DnsError, ADDRS_MAX, MESSAGE_MAX};
use std::net::Ipv4Addr;

const LABEL: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-";

fn hostname(tc: &TestCase) -> String {
    let labels: Vec<String> = (0..tc.draw(gs::integers::<usize>().min_value(1).max_value(5)))
        .map(|_| {
            tc.draw(
                gs::vecs(
                    gs::integers::<usize>()
                        .min_value(0)
                        .max_value(LABEL.len() - 1),
                )
                .min_size(1)
                .max_size(20),
            )
            .into_iter()
            .map(|i| LABEL[i] as char)
            .collect()
        })
        .collect();
    labels.join(".")
}

/// Turn an encoded query into a response carrying `addrs`, each owned by the
/// question name through a compression pointer.
fn answer(query: &[u8], addrs: &[Ipv4Addr]) -> Vec<u8> {
    let mut m = query.to_vec();
    m[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
    m[6..8].copy_from_slice(&(addrs.len() as u16).to_be_bytes());
    for a in addrs {
        m.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        m.extend_from_slice(&a.octets());
    }
    m
}

/// Arbitrary datagrams never panic, and any accepted answer is well-formed.
#[hegel::test]
fn parsing_arbitrary_datagrams_is_safe(tc: TestCase) {
    let data = tc.draw(gs::binary().max_size(600));
    let id = tc.draw(gs::integers::<u16>());
    if let Ok(a) = parse_response(&data, id, "s3.example.com") {
        assert!((1..=ADDRS_MAX).contains(&a.len));
    }
}

/// The addresses a server sends for the name we asked about are exactly the
/// addresses we cache, in order, up to the fixed maximum.
#[hegel::test]
fn answers_round_trip(tc: TestCase) {
    let name = hostname(&tc);
    let id = tc.draw(gs::integers::<u16>());
    let addrs: Vec<Ipv4Addr> = tc
        .draw(gs::vecs(gs::integers::<u32>()).min_size(1).max_size(12))
        .into_iter()
        .map(Ipv4Addr::from)
        .collect();

    let mut q = [0u8; MESSAGE_MAX];
    let n = encode_query(&name, id, &mut q).expect("generated hostnames are valid");
    let got = parse_response(&answer(&q[..n], &addrs), id, &name)
        .expect("a well-formed answer must parse");
    assert_eq!(got.as_slice(), &addrs[..addrs.len().min(ADDRS_MAX)]);

    // Case does not matter; the id and the name do.
    assert!(parse_response(&answer(&q[..n], &addrs), id, &name.to_uppercase()).is_ok());
    assert_eq!(
        parse_response(&answer(&q[..n], &addrs), id.wrapping_add(1), &name),
        Err(DnsError::IdMismatch)
    );
    let other = format!("x{name}");
    if encode_query(&other, id, &mut [0u8; MESSAGE_MAX]).is_ok() {
        assert_eq!(
            parse_response(&answer(&q[..n], &addrs), id, &other),
            Err(DnsError::QuestionMismatch)
        );
    }
}

/// Any name the encoder accepts produces a query the parser recognises as a
/// question for that same name; everything else is refused, never mangled.
#[hegel::test]
fn encoder_accepts_or_refuses_cleanly(tc: TestCase) {
    let name = tc.draw(gs::text().max_size(80));
    let mut q = [0u8; MESSAGE_MAX];
    match encode_query(&name, 7, &mut q) {
        Err(e) => assert_eq!(e, DnsError::BadName),
        Ok(n) => {
            assert!(n <= MESSAGE_MAX);
            let got = parse_response(&answer(&q[..n], &[Ipv4Addr::new(192, 0, 2, 1)]), 7, &name);
            assert_eq!(
                got.map(|a| a.len),
                Ok(1),
                "accepted name {name:?} did not survive a round trip"
            );
        }
    }
}

/// Truncating a valid answer anywhere yields an error, never a partial answer.
#[hegel::test]
fn truncated_answers_are_rejected(tc: TestCase) {
    let name = hostname(&tc);
    let addrs: Vec<Ipv4Addr> = tc
        .draw(gs::vecs(gs::integers::<u32>()).min_size(1).max_size(8))
        .into_iter()
        .map(Ipv4Addr::from)
        .collect();
    let mut q = [0u8; MESSAGE_MAX];
    let n = encode_query(&name, 1, &mut q).unwrap();
    let full = answer(&q[..n], &addrs);
    let cut = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(full.len() - 1),
    );
    assert!(
        parse_response(&full[..cut], 1, &name).is_err(),
        "accepted an answer cut at {cut} of {}",
        full.len()
    );
}
