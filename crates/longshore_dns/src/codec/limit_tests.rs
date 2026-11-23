// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The codec at its limits: the longest label, name and pointer chain it
//! takes and the first it does not, the smallest buffer a query fits, label
//! types that do not exist, and the edges of what resolv.conf may say.

use super::tests::response;
use super::*;

fn query(name: &str, id: u16) -> Vec<u8> {
    let mut b = [0u8; MESSAGE_MAX];
    let n = encode_query(name, id, &mut b).unwrap();
    b[..n].to_vec()
}

/// Append an IN record with the owner bytes given.
fn record(m: &mut Vec<u8>, owner: &[u8], rtype: u16, rdata: &[u8]) {
    m.extend_from_slice(owner);
    m.extend_from_slice(&rtype.to_be_bytes());
    m.extend_from_slice(&CLASS_IN.to_be_bytes());
    m.extend_from_slice(&300u32.to_be_bytes());
    m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    m.extend_from_slice(rdata);
}

fn pointer(to: usize) -> [u8; 2] {
    (0xc000 | to as u16).to_be_bytes()
}

/// An answer for `x.io` whose owner name reaches the question through
/// `hops` pointers, each to the one before, kept in an unrelated record.
fn answer_through_pointers(hops: usize) -> Vec<u8> {
    let q = query("x.io", 7);
    let mut r = response(&q, 0x8180, &[]);
    // A TXT record whose data is the chain: the first pointer is to the
    // question, each later one to the pointer two bytes before it.
    let chain_at = r.len() + 12;
    let mut chain = Vec::new();
    for hop in 0..hops - 1 {
        let to = if hop == 0 {
            12
        } else {
            chain_at + 2 * (hop - 1)
        };
        chain.extend_from_slice(&pointer(to));
    }
    record(&mut r, &pointer(12), 16, &chain);
    // The owner's own pointer is the last hop.
    let owner = if hops == 1 {
        pointer(12)
    } else {
        pointer(chain_at + 2 * (hops - 2))
    };
    record(&mut r, &owner, 1, &[192, 0, 2, 7]);
    r[7] = 2;
    r
}

#[test]
fn a_name_may_follow_sixteen_pointers_and_no_more() {
    for hops in [1, 2, 15, JUMPS_MAX] {
        assert_eq!(
            parse_response(&answer_through_pointers(hops), 7, "x.io").map(|a| a.len),
            Ok(1),
            "{hops} pointers"
        );
    }
    let a = parse_response(&answer_through_pointers(JUMPS_MAX), 7, "x.io").unwrap();
    assert_eq!(a.as_slice(), [Ipv4Addr::new(192, 0, 2, 7)]);
    for hops in [JUMPS_MAX + 1, JUMPS_MAX + 2, 40] {
        assert_eq!(
            parse_response(&answer_through_pointers(hops), 7, "x.io"),
            Err(DnsError::BadCompression),
            "{hops} pointers"
        );
    }
}

#[test]
fn label_types_that_do_not_exist_are_refused() {
    let q = query("x.io", 7);
    // In the question: the first length byte with one of the two high bits.
    for bad in [0x40u8, 0x41, 0x7f, 0x80, 0xbf] {
        let mut r = response(&q, 0x8180, &[(1, &[1, 2, 3, 4])]);
        r[12] = bad;
        assert_eq!(
            parse_response(&r, 7, "x.io"),
            Err(DnsError::BadCompression),
            "question label {bad:#x}"
        );
        // And as an answer's owner name.
        let mut bad_owner = response(&q, 0x8180, &[]);
        record(&mut bad_owner, &[bad, b'x', 0], 1, &[1, 2, 3, 4]);
        bad_owner[7] = 1;
        assert_eq!(
            parse_response(&bad_owner, 7, "x.io"),
            Err(DnsError::BadCompression),
            "owner label {bad:#x}"
        );
    }
}

#[test]
fn an_owner_spelled_out_in_another_case_is_the_same_name() {
    let q = query("x.io", 7);
    let mut r = response(&q, 0x8180, &[]);
    record(&mut r, &[1, b'X', 2, b'I', b'o', 0], 1, &[192, 0, 2, 7]);
    // A longer name that only starts the same is another name.
    record(
        &mut r,
        &[1, b'x', 2, b'i', b'o', 1, b'z', 0],
        1,
        &[6, 6, 6, 6],
    );
    record(&mut r, &[1, b'x', 0], 1, &[6, 6, 6, 6]);
    r[7] = 3;
    assert_eq!(
        parse_response(&r, 7, "x.io").unwrap().as_slice(),
        [Ipv4Addr::new(192, 0, 2, 7)]
    );
}

#[test]
fn records_of_another_class_are_not_addresses() {
    let q = query("x.io", 7);
    let mut r = response(&q, 0x8180, &[(1, &[192, 0, 2, 7])]);
    // The class is the two bytes after the owner pointer and the type.
    let class_at = q.len() + 4;
    assert_eq!(r[class_at..class_at + 2], [0, 1]);
    r[class_at + 1] = 3; // CHAOS
    assert_eq!(parse_response(&r, 7, "x.io"), Err(DnsError::NoAddresses));
    // Nor is a question of another class ours, or two questions.
    let mut question_class = response(&q, 0x8180, &[(1, &[192, 0, 2, 7])]);
    question_class[q.len() - 1] = 3;
    assert_eq!(
        parse_response(&question_class, 7, "x.io"),
        Err(DnsError::QuestionMismatch)
    );
    let mut two_questions = response(&q, 0x8180, &[(1, &[192, 0, 2, 7])]);
    two_questions[5] = 2;
    assert_eq!(
        parse_response(&two_questions, 7, "x.io"),
        Err(DnsError::QuestionMismatch)
    );
}

#[test]
fn every_response_code_but_zero_is_reported() {
    let q = query("x.io", 9);
    for rcode in 1..=15u8 {
        assert_eq!(
            parse_response(&response(&q, 0x8180 | u16::from(rcode), &[]), 9, "x.io"),
            Err(DnsError::Rcode(rcode))
        );
    }
}

#[test]
fn labels_and_names_are_taken_up_to_their_limits() {
    let mut b = [0u8; MESSAGE_MAX];
    let label63 = "a".repeat(63);
    assert_eq!(encode_query(&label63, 1, &mut b), Ok(12 + 63 + 2 + 4));
    assert_eq!(b[12], 63);
    assert_eq!(
        encode_query(&"a".repeat(64), 1, &mut b),
        Err(DnsError::BadName)
    );
    // 63 + 63 + 63 + 61 letters and three dots: 253 bytes, the longest name.
    let longest = format!("{label63}.{label63}.{label63}.{}", "b".repeat(61));
    assert_eq!(longest.len(), 253);
    assert_eq!(encode_query(&longest, 1, &mut b), Ok(12 + 253 + 2 + 4));
    assert_eq!(
        encode_query(&format!("{longest}."), 1, &mut b),
        Ok(271),
        "a trailing dot is not part of the name"
    );
    let r = response(&b[..271], 0x8180, &[(1, &[192, 0, 2, 7])]);
    assert_eq!(parse_response(&r, 1, &longest).map(|a| a.len), Ok(1));
    let too_long = format!("{label63}.{label63}.{label63}.{}", "b".repeat(62));
    assert_eq!(encode_query(&too_long, 1, &mut b), Err(DnsError::BadName));
}

#[test]
fn a_query_fits_a_buffer_of_exactly_its_length() {
    // Header, the name with its length bytes and root, type and class.
    let need = 12 + "example.com".len() + 2 + 4;
    let mut exact = vec![0xeeu8; need + 3];
    assert_eq!(encode_query("example.com", 1, &mut exact[..need]), Ok(need));
    assert_eq!(exact[need..], [0xee; 3], "nothing written past the query");
    let mut short = vec![0xeeu8; need - 1];
    assert_eq!(
        encode_query("example.com", 1, &mut short),
        Err(DnsError::BadName)
    );
    assert!(short.iter().all(|b| *b == 0xee), "nothing written at all");
    assert_eq!(encode_query("a", 1, &mut []), Err(DnsError::BadName));
}

#[test]
fn names_may_use_hyphens_and_underscores_and_nothing_stranger() {
    let mut b = [0u8; MESSAGE_MAX];
    for good in ["a_b.example", "-.com.", "_dmarc.x-y.io", "0.1", "A"] {
        assert!(encode_query(good, 1, &mut b).is_ok(), "{good:?}");
    }
    for bad in [
        "a b", "a,b.io", "é.io", "a.b..", "..", ".a", "a/b", "a\0b", "a.b.\n",
    ] {
        assert_eq!(
            encode_query(bad, 1, &mut b),
            Err(DnsError::BadName),
            "{bad:?}"
        );
    }
}

#[test]
fn resolv_conf_edges() {
    let ips = |text: &str| parse_resolv_conf(text);
    let v6 = |s: &str| s.parse::<IpAddr>().unwrap();
    assert_eq!(ips(""), [] as [IpAddr; 0]);
    // Link-local is fe80::/10: up to febf, and no further.
    assert_eq!(
        ips("nameserver fe80::1\nnameserver febf::1\nnameserver fec0::1\nnameserver fe7f::1\n"),
        [v6("fec0::1"), v6("fe7f::1")]
    );
    // Lines that name no address are passed over.
    assert_eq!(
        ips("nameserver\nnameserver bogus\nnameserver 300.1.1.1\nnameserverx 1.1.1.1\nNAMESERVER 1.1.1.1\n#nameserver 1.1.1.1\n"),
        [] as [IpAddr; 0]
    );
    // Windows line endings, tabs, trailing words, and no final newline.
    assert_eq!(
        ips("nameserver 192.0.2.1\r\nnameserver\t192.0.2.2 # the second\r\nnameserver 192.0.2.3"),
        [
            IpAddr::from([192, 0, 2, 1]),
            IpAddr::from([192, 0, 2, 2]),
            IpAddr::from([192, 0, 2, 3]),
        ]
    );
    // An IPv4 address is never link-local to this rule, whatever it is.
    assert_eq!(
        ips("nameserver 169.254.1.1\nnameserver ::1\n"),
        [IpAddr::from([169, 254, 1, 1]), v6("::1")]
    );
}
