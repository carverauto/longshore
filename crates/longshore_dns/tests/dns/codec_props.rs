// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests for the DNS codec (Hegel).

use hegel::generators as gs;
use hegel::TestCase;
use longshore_dns::codec::{
    encode_query, encode_query_for, parse_response, parse_response_for, DnsError, Record,
    ADDRS_MAX, MESSAGE_MAX,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

const NAME_BYTES: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";

/// A name of any shape the encoder takes, up to and past its limits: labels
/// of 1 to 63 bytes, any total length, sometimes with a trailing dot.
fn long_hostname(tc: &TestCase) -> String {
    let labels: Vec<String> = (0..tc.draw(gs::integers::<usize>().min_value(1).max_value(6)))
        .map(|_| {
            let len = tc.draw(gs::integers::<usize>().min_value(1).max_value(63));
            let letter = tc.draw(gs::integers::<usize>().max_value(NAME_BYTES.len() - 1));
            // One drawn letter, then the alphabet in turn: long labels cheaply.
            (0..len)
                .map(|i| NAME_BYTES[(letter + i) % NAME_BYTES.len()] as char)
                .collect()
        })
        .collect();
    let dot = if tc.draw(gs::booleans()) { "." } else { "" };
    format!("{}{dot}", labels.join("."))
}

/// A valid name is encoded exactly when the buffer has room for it, to
/// exactly the length a query for it has, and nothing is written past that.
/// A name over 253 bytes is refused whatever the room.
#[hegel::test]
fn a_query_is_written_when_it_fits_and_refused_when_it_does_not(tc: TestCase) {
    let name = long_hostname(&tc);
    let bare = name.strip_suffix('.').unwrap_or(&name).len();
    let room = tc.draw(gs::integers::<usize>().max_value(300));
    let record = tc.draw(gs::sampled_from(vec![Record::A, Record::Aaaa]));
    let mut out = vec![0xeeu8; room + 4];
    let need = 12 + bare + 2 + 4;
    let got = encode_query_for(&name, 0x1234, record, &mut out[..room]);
    if bare > 253 || room < need {
        assert_eq!(got, Err(DnsError::BadName));
        return;
    }
    assert_eq!(got, Ok(need));
    assert!(
        out[need..].iter().all(|b| *b == 0xee),
        "wrote past the query"
    );
    // The parser reads the same name back out of it.
    let (rtype, rdata): (u8, &[u8]) = match record {
        Record::A => (1, &[192, 0, 2, 1]),
        Record::Aaaa => (
            28,
            &[0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        ),
    };
    let mut m = out[..need].to_vec();
    m[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
    m[7] = 1;
    m.extend_from_slice(&[0xc0, 12, 0, rtype, 0, 1, 0, 0, 0, 60, 0, rdata.len() as u8]);
    m.extend_from_slice(rdata);
    assert_eq!(
        parse_response_for(&m, 0x1234, &name, record).map(|a| a.len),
        Ok(1)
    );
}

/// What follows a well-formed question may be anything at all. The parser
/// gets past the header and the question, so the answer section is what is
/// under test: it never panics, never returns more addresses than the
/// header counts, and every address it returns is in the datagram.
#[hegel::test]
fn arbitrary_answer_sections_are_safe(tc: TestCase) {
    let name = hostname(&tc);
    let record = tc.draw(gs::sampled_from(vec![Record::A, Record::Aaaa]));
    let mut q = [0u8; MESSAGE_MAX];
    let n = encode_query_for(&name, 7, record, &mut q).unwrap();
    let mut m = q[..n].to_vec();
    m[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
    let count = tc.draw(gs::integers::<u16>().max_value(20));
    m[6..8].copy_from_slice(&count.to_be_bytes());
    let tail = tc.draw(gs::binary().max_size(400));
    m.extend_from_slice(&tail);
    match parse_response_for(&m, 7, &name, record) {
        Ok(a) => {
            assert!((1..=ADDRS_MAX.min(count as usize)).contains(&a.len));
            for addr in a.as_slice() {
                let octets = match addr {
                    IpAddr::V4(a) => a.octets().to_vec(),
                    IpAddr::V6(a) => a.octets().to_vec(),
                };
                assert_eq!(addr.is_ipv6(), record == Record::Aaaa);
                assert!(
                    tail.windows(octets.len()).any(|w| w == octets),
                    "{addr} is nowhere in the answer section"
                );
            }
        }
        Err(e) => assert!(
            matches!(
                e,
                DnsError::Truncated | DnsError::BadCompression | DnsError::NoAddresses
            ),
            "{e:?} from an answer section"
        ),
    }
}

/// One record of a generated answer section.
struct Rec {
    rtype: u16,
    class: u16,
    rdata: Vec<u8>,
    /// Whether the owner is the question's name.
    ours: bool,
}

/// Answer sections made of whole records, of any type, class and length,
/// owned by the question's name or by another. The parser returns exactly
/// the addresses of the right type, class and length that the question's
/// name owns, in order, up to the maximum. No CNAME is among them, so the
/// name asked for is the only one that counts.
#[hegel::test]
fn only_address_records_of_the_name_asked_for_are_returned(tc: TestCase) {
    let name = hostname(&tc);
    let record = tc.draw(gs::sampled_from(vec![Record::A, Record::Aaaa]));
    let (code, len) = match record {
        Record::A => (1u16, 4usize),
        Record::Aaaa => (28, 16),
    };
    let mut q = [0u8; MESSAGE_MAX];
    let n = encode_query_for(&name, 7, record, &mut q).unwrap();
    let mut m = q[..n].to_vec();
    m[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
    let count = tc.draw(gs::integers::<usize>().max_value(12));
    m[6..8].copy_from_slice(&(count as u16).to_be_bytes());
    let mut recs = Vec::new();
    for _ in 0..count {
        let rec = Rec {
            // A, AAAA, and types that carry no address (NS, TXT, anything).
            rtype: match tc.draw(gs::integers::<u8>().max_value(5)) {
                0 | 1 => 1,
                2 | 3 => 28,
                4 => tc.draw(gs::sampled_from(vec![2u16, 16, 33, 41, 255])),
                _ => tc.draw(gs::integers::<u16>().min_value(6)),
            },
            class: if tc.draw(gs::integers::<u8>().max_value(5)) == 0 {
                tc.draw(gs::integers::<u16>())
            } else {
                1
            },
            rdata: match tc.draw(gs::integers::<u8>().max_value(5)) {
                0 => tc.draw(gs::binary().max_size(20)),
                1 | 2 => tc.draw(gs::binary().min_size(16).max_size(16)),
                _ => tc.draw(gs::binary().min_size(4).max_size(4)),
            },
            ours: tc.draw(gs::integers::<u8>().max_value(3)) != 0,
        };
        if rec.ours {
            m.extend_from_slice(&[0xc0, 12]);
        } else {
            // The question's name under another first label.
            m.extend_from_slice(&[5, b'o', b't', b'h', b'e', b'r', 0xc0, 12]);
        }
        m.extend_from_slice(&rec.rtype.to_be_bytes());
        m.extend_from_slice(&rec.class.to_be_bytes());
        m.extend_from_slice(&[0, 0, 0, 60, 0, rec.rdata.len() as u8]);
        m.extend_from_slice(&rec.rdata);
        recs.push(rec);
    }
    let want: Vec<IpAddr> = recs
        .iter()
        .filter(|r| r.ours && r.rtype == code && r.class == 1 && r.rdata.len() == len)
        .take(ADDRS_MAX)
        .map(|r| match record {
            Record::A => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(&r.rdata[..]).unwrap())),
            Record::Aaaa => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&r.rdata[..]).unwrap())),
        })
        .collect();
    match parse_response_for(&m, 7, &name, record) {
        Ok(got) => assert_eq!(got.as_slice(), &want[..]),
        Err(e) => {
            assert_eq!(e, DnsError::NoAddresses);
            assert!(want.is_empty(), "lost {want:?}");
        }
    }
    // Cut anywhere inside the answer section, it is refused or holds only
    // addresses from the front of the whole answer.
    let cut = tc.draw(gs::integers::<usize>().min_value(n).max_value(m.len()));
    if let Ok(got) = parse_response_for(&m[..cut], 7, &name, record) {
        assert_eq!(
            cut,
            m.len(),
            "a cut answer was accepted: {:?}",
            got.as_slice()
        );
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

/// A response may mix A and AAAA records. A query for one family gets that
/// family's addresses, in order, and never one of the other family.
#[hegel::test]
fn mixed_answers_are_split_by_family(tc: TestCase) {
    let name = hostname(&tc);
    let id = tc.draw(gs::integers::<u16>());
    let addrs: Vec<IpAddr> = tc
        .draw(gs::vecs(gs::integers::<u128>()).min_size(1).max_size(12))
        .into_iter()
        // Odd values become IPv6 addresses, even ones IPv4.
        .map(|v| match v & 1 {
            1 => IpAddr::V6(Ipv6Addr::from(v)),
            _ => IpAddr::V4(Ipv4Addr::from((v >> 64) as u32)),
        })
        .collect();
    for record in [Record::A, Record::Aaaa] {
        let mut q = [0u8; MESSAGE_MAX];
        let n = encode_query_for(&name, id, record, &mut q).unwrap();
        let mut m = q[..n].to_vec();
        m[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
        m[6..8].copy_from_slice(&(addrs.len() as u16).to_be_bytes());
        for a in &addrs {
            match a {
                IpAddr::V4(a) => {
                    m.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
                    m.extend_from_slice(&a.octets());
                }
                IpAddr::V6(a) => {
                    m.extend_from_slice(&[0xc0, 12, 0, 28, 0, 1, 0, 0, 0, 60, 0, 16]);
                    m.extend_from_slice(&a.octets());
                }
            }
        }
        let want: Vec<IpAddr> = addrs
            .iter()
            .copied()
            .filter(|a| a.is_ipv6() == (record == Record::Aaaa))
            .take(ADDRS_MAX)
            .collect();
        match parse_response_for(&m, id, &name, record) {
            Ok(got) => assert_eq!(got.as_slice(), &want[..]),
            Err(e) => {
                assert_eq!(e, DnsError::NoAddresses);
                assert!(want.is_empty());
            }
        }
    }
}
