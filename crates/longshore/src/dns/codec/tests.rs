// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Minimal response builder for tests: echoes the question, then `answers`
/// as (type, rdata) with the owner name compressed to offset 12.
pub(crate) fn response(query: &[u8], flags: u16, answers: &[(u16, &[u8])]) -> Vec<u8> {
    let mut m = query.to_vec();
    m[2..4].copy_from_slice(&flags.to_be_bytes());
    m[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
    for (t, rdata) in answers {
        m.extend_from_slice(&[0xc0, 12]);
        m.extend_from_slice(&t.to_be_bytes());
        m.extend_from_slice(&CLASS_IN.to_be_bytes());
        m.extend_from_slice(&300u32.to_be_bytes());
        m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        m.extend_from_slice(rdata);
    }
    m
}

fn query(name: &str, id: u16) -> Vec<u8> {
    let mut b = [0u8; MESSAGE_MAX];
    let n = encode_query(name, id, &mut b).unwrap();
    b[..n].to_vec()
}

#[test]
fn encodes_a_known_query() {
    let q = query("a.example.com", 0x1234);
    assert_eq!(
        q,
        [
            0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'a', 7, b'e', b'x', b'a', b'm', b'p',
            b'l', b'e', 3, b'c', b'o', b'm', 0, 0, 1, 0, 1
        ]
    );
    assert_eq!(
        query("a.example.com.", 0x1234),
        q,
        "trailing dot is the same name"
    );
}

#[test]
fn rejects_bad_names() {
    let mut b = [0u8; MESSAGE_MAX];
    for bad in [
        "",
        ".",
        "a..b",
        "has space.com",
        &"x".repeat(64),
        &format!("{}.com", "a.".repeat(130)),
    ] {
        assert_eq!(
            encode_query(bad, 1, &mut b),
            Err(DnsError::BadName),
            "{bad:?}"
        );
    }
    assert_eq!(
        encode_query("example.com", 1, &mut [0u8; 20]),
        Err(DnsError::BadName),
        "buffer too small"
    );
}

/// Append an IN record whose owner is a pointer to `owner`.
fn record(m: &mut Vec<u8>, owner: u16, rtype: u16, rdata: &[u8]) {
    m.extend_from_slice(&(0xc000 | owner).to_be_bytes());
    m.extend_from_slice(&rtype.to_be_bytes());
    m.extend_from_slice(&CLASS_IN.to_be_bytes());
    m.extend_from_slice(&300u32.to_be_bytes());
    m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    m.extend_from_slice(rdata);
}

#[test]
fn follows_cname_to_the_addresses_of_its_target() {
    // s3.example.com CNAME foo.s3.example.com, whose A records follow.
    let q = query("s3.example.com", 7);
    let mut r = response(&q, 0x8180, &[(5, &[3, b'f', b'o', b'o', 0xc0, 12])]);
    let target = (q.len() + 12) as u16;
    record(&mut r, target, 1, &[192, 0, 2, 1]);
    record(&mut r, target, 1, &[192, 0, 2, 2]);
    r[7] = 3;
    let a = parse_response(&r, 7, "S3.Example.COM").unwrap();
    assert_eq!(
        a.as_slice(),
        [Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)]
    );
}

#[test]
fn ignores_addresses_owned_by_other_names() {
    // Answer: other.io A 6.6.6.6 for a question about x.io.
    let q = query("x.io", 7);
    let mut r = response(&q, 0x8180, &[]);
    let other = r.len() as u16;
    r.extend_from_slice(&[5, b'o', b't', b'h', b'e', b'r', 0xc0, 14]);
    r.extend_from_slice(&1u16.to_be_bytes());
    r.extend_from_slice(&CLASS_IN.to_be_bytes());
    r.extend_from_slice(&300u32.to_be_bytes());
    r.extend_from_slice(&[0, 4, 6, 6, 6, 6]);
    r[7] = 1;
    assert_eq!(parse_response(&r, 7, "x.io"), Err(DnsError::NoAddresses));
    // A CNAME from a name other than the question does not start a chain.
    let mut r = response(&q, 0x8180, &[]);
    record(&mut r, 14, 5, &[5, b'o', b't', b'h', b'e', b'r', 0]);
    let target = other + 12;
    record(&mut r, target, 1, &[6, 6, 6, 6]);
    r[7] = 2;
    assert_eq!(parse_response(&r, 7, "x.io"), Err(DnsError::NoAddresses));
}

#[test]
fn rejects_answer_owner_compression_loops() {
    let q = query("x.io", 7);
    let mut r = response(&q, 0x8180, &[]);
    let own = r.len() as u16;
    record(&mut r, own, 1, &[1, 2, 3, 4]);
    r[7] = 1;
    assert_eq!(
        parse_response(&r, 7, "x.io"),
        Err(DnsError::BadCompression),
        "self pointer"
    );
    let end = r.len() as u16 + 4;
    r.truncate(own as usize);
    record(&mut r, end, 1, &[1, 2, 3, 4]);
    assert_eq!(
        parse_response(&r, 7, "x.io"),
        Err(DnsError::BadCompression),
        "forward pointer"
    );
}

#[test]
fn caps_addresses_at_the_fixed_maximum() {
    let q = query("x.io", 1);
    let recs: Vec<(u16, &[u8])> = (0..20).map(|_| (1u16, &[1u8, 2, 3, 4][..])).collect();
    assert_eq!(
        parse_response(&response(&q, 0x8180, &recs), 1, "x.io")
            .unwrap()
            .len,
        ADDRS_MAX
    );
}

#[test]
fn reports_each_rejection() {
    let q = query("x.io", 9);
    let ok = response(&q, 0x8180, &[(1, &[1, 2, 3, 4])]);
    assert_eq!(parse_response(&ok, 8, "x.io"), Err(DnsError::IdMismatch));
    assert_eq!(
        parse_response(&ok, 9, "y.io"),
        Err(DnsError::QuestionMismatch)
    );
    assert_eq!(parse_response(&q, 9, "x.io"), Err(DnsError::NotAResponse));
    assert_eq!(
        parse_response(&response(&q, 0x8183, &[]), 9, "x.io"),
        Err(DnsError::Rcode(3))
    );
    assert_eq!(
        parse_response(&response(&q, 0x8380, &[]), 9, "x.io"),
        Err(DnsError::TruncatedByServer)
    );
    assert_eq!(
        parse_response(&response(&q, 0x8180, &[]), 9, "x.io"),
        Err(DnsError::NoAddresses)
    );
    assert_eq!(
        parse_response(&response(&q, 0x8180, &[(5, &[0xc0, 12])]), 9, "x.io"),
        Err(DnsError::NoAddresses)
    );
    for cut in 0..ok.len() {
        assert!(
            parse_response(&ok[..cut], 9, "x.io").is_err(),
            "accepted a message cut at {cut}"
        );
    }
}

#[test]
fn compression_loops_are_bounded() {
    // Question name is a pointer to itself, then a forward pointer, then a
    // long backward chain that exceeds the jump limit.
    let mut m = vec![
        0, 9, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1,
    ];
    assert_eq!(parse_response(&m, 9, "x.io"), Err(DnsError::BadCompression));
    m[13] = 40;
    assert_eq!(parse_response(&m, 9, "x.io"), Err(DnsError::BadCompression));
}

#[test]
fn random_mutations_never_panic_or_read_out_of_bounds() {
    // Cheap in-tree fuzzing; the cargo-fuzz target explores further.
    let q = query("bucket.s3.example.com", 0xbeef);
    let cname = [3, b'f', b'o', b'o', 0xc0, 12];
    let base = response(
        &q,
        0x8180,
        &[(5, &cname), (1, &[192, 0, 2, 1]), (1, &[192, 0, 2, 2])],
    );
    let mut s = 0x243f_6a88_85a3_08d3u64;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for _ in 0..200_000 {
        let mut m = base.clone();
        for _ in 0..1 + rnd() % 4 {
            let i = (rnd() % m.len() as u64) as usize;
            m[i] = rnd() as u8;
        }
        m.truncate(1 + (rnd() % m.len() as u64) as usize + (rnd() % 2) as usize * m.len());
        let _ = parse_response(&m, 0xbeef, "bucket.s3.example.com");
    }
}

#[test]
fn reads_resolv_conf() {
    let text = "# comment\nsearch lan\nnameserver 192.0.2.53\nnameserver fe80::1\nnameserver fe80::1%eth0\n  nameserver   1.1.1.1  \nnameserver 2001:db8::53\noptions edns0\n";
    assert_eq!(
        parse_resolv_conf(text),
        [
            IpAddr::from([192, 0, 2, 53]),
            IpAddr::from([1, 1, 1, 1]),
            "2001:db8::53".parse::<IpAddr>().unwrap(),
        ]
    );
}

fn query_aaaa(name: &str, id: u16) -> Vec<u8> {
    let mut b = [0u8; MESSAGE_MAX];
    let n = encode_query_for(name, id, Record::Aaaa, &mut b).unwrap();
    b[..n].to_vec()
}

const V6: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

#[test]
fn an_aaaa_query_differs_from_an_a_query_only_in_its_type() {
    let (a, aaaa) = (query("a.example.com", 7), query_aaaa("a.example.com", 7));
    let n = a.len();
    assert_eq!(a[..n - 4], aaaa[..n - 4]);
    assert_eq!(
        (&a[n - 4..], &aaaa[n - 4..]),
        (&[0, 1, 0, 1][..], &[0, 28, 0, 1][..])
    );
}

#[test]
fn parses_aaaa_records_and_follows_a_cname_to_them() {
    let q = query_aaaa("s3.example.com", 7);
    let mut r = response(&q, 0x8180, &[(5, &[3, b'f', b'o', b'o', 0xc0, 12])]);
    let target = (q.len() + 12) as u16;
    record(&mut r, target, 28, &V6);
    r[7] = 2;
    let a = parse_response_for(&r, 7, "s3.example.com", Record::Aaaa).unwrap();
    assert_eq!(a.as_slice(), [IpAddr::V6(Ipv6Addr::from(V6))]);
}

#[test]
fn each_query_keeps_only_addresses_of_its_own_family() {
    // A server that answers with both record types whatever was asked.
    let both: [(u16, &[u8]); 2] = [(1, &[192, 0, 2, 1]), (28, &V6)];
    let r = response(&query_aaaa("x.io", 7), 0x8180, &both);
    assert_eq!(
        parse_response_for(&r, 7, "x.io", Record::Aaaa)
            .unwrap()
            .as_slice(),
        [IpAddr::V6(Ipv6Addr::from(V6))]
    );
    let r = response(&query("x.io", 7), 0x8180, &both);
    assert_eq!(
        parse_response(&r, 7, "x.io").unwrap().as_slice(),
        [Ipv4Addr::new(192, 0, 2, 1)]
    );
}

#[test]
fn an_answer_to_the_other_familys_question_is_not_ours() {
    let r = response(&query("x.io", 7), 0x8180, &[(1, &[192, 0, 2, 1])]);
    assert_eq!(
        parse_response_for(&r, 7, "x.io", Record::Aaaa),
        Err(DnsError::QuestionMismatch)
    );
}

#[test]
fn address_records_of_the_wrong_length_are_skipped() {
    let q = query_aaaa("x.io", 7);
    let r = response(&q, 0x8180, &[(28, &V6[..4]), (28, &V6[..15])]);
    assert_eq!(
        parse_response_for(&r, 7, "x.io", Record::Aaaa),
        Err(DnsError::NoAddresses)
    );
    let r = response(&query("x.io", 7), 0x8180, &[(1, &V6)]);
    assert_eq!(parse_response(&r, 7, "x.io"), Err(DnsError::NoAddresses));
}
