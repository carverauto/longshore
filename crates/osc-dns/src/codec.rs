// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! DNS message encoding and parsing (RFC 1035), A records only.

use std::net::Ipv4Addr;

pub const ADDRS_MAX: usize = 8;
/// Largest message we send or accept over UDP without EDNS.
pub const MESSAGE_MAX: usize = 512;
/// Compression pointers followed before a name is rejected.
const JUMPS_MAX: usize = 16;

const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const CLASS_IN: u16 = 1;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DnsError {
    /// The hostname is not a valid DNS name, or does not fit the buffer.
    BadName,
    /// The message ends before a field it declares.
    Truncated,
    NotAResponse,
    IdMismatch,
    /// The server set the truncation bit; the answer did not fit in UDP.
    TruncatedByServer,
    /// Non-zero response code (3 is NXDOMAIN).
    Rcode(u8),
    /// The question echoed back is not the one we asked.
    QuestionMismatch,
    /// A compression pointer loop or forward pointer.
    BadCompression,
    NoAddresses,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Answers {
    pub addrs: [Ipv4Addr; ADDRS_MAX],
    pub len: usize,
}

impl Answers {
    pub fn as_slice(&self) -> &[Ipv4Addr] {
        &self.addrs[..self.len]
    }
}

/// Write a recursive A query for `name`. Returns the message length.
pub fn encode_query(name: &str, id: u16, out: &mut [u8]) -> Result<usize, DnsError> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.len() > 253 || out.len() < 12 + name.len() + 2 + 4 {
        return Err(DnsError::BadName);
    }
    out[..2].copy_from_slice(&id.to_be_bytes());
    out[2..4].copy_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    out[4..6].copy_from_slice(&1u16.to_be_bytes()); // one question
    out[6..12].fill(0);
    let mut at = 12;
    for label in name.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(DnsError::BadName);
        }
        out[at] = label.len() as u8;
        out[at + 1..at + 1 + label.len()].copy_from_slice(label.as_bytes());
        at += 1 + label.len();
    }
    out[at] = 0;
    out[at + 1..at + 3].copy_from_slice(&TYPE_A.to_be_bytes());
    out[at + 3..at + 5].copy_from_slice(&CLASS_IN.to_be_bytes());
    Ok(at + 5)
}

fn u16_at(m: &[u8], at: usize) -> Result<u16, DnsError> {
    m.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or(DnsError::Truncated)
}

/// Offset just past the name starting at `at`. Never follows pointers, so it
/// cannot loop: a pointer ends the name in place.
fn skip_name(m: &[u8], mut at: usize) -> Result<usize, DnsError> {
    loop {
        let len = *m.get(at).ok_or(DnsError::Truncated)? as usize;
        match len {
            0 => return Ok(at + 1),
            l if l & 0xc0 == 0xc0 => {
                return if at + 2 <= m.len() {
                    Ok(at + 2)
                } else {
                    Err(DnsError::Truncated)
                }
            }
            l if l & 0xc0 != 0 => return Err(DnsError::BadCompression),
            l => at += 1 + l,
        }
    }
}

/// The next label of the name being read at `at`, or None at its end.
/// Follows compression pointers, backwards only and at most `JUMPS_MAX`
/// times per name.
fn next_label<'m>(
    m: &'m [u8],
    at: &mut usize,
    jumps: &mut usize,
) -> Result<Option<&'m [u8]>, DnsError> {
    loop {
        let len = *m.get(*at).ok_or(DnsError::Truncated)? as usize;
        if len == 0 {
            return Ok(None);
        }
        if len & 0xc0 == 0xc0 {
            let target = (u16_at(m, *at)? & 0x3fff) as usize;
            *jumps += 1;
            if target >= *at || *jumps > JUMPS_MAX {
                return Err(DnsError::BadCompression);
            }
            *at = target;
            continue;
        }
        if len & 0xc0 != 0 {
            return Err(DnsError::BadCompression);
        }
        let label = m.get(*at + 1..*at + 1 + len).ok_or(DnsError::Truncated)?;
        *at += 1 + len;
        return Ok(Some(label));
    }
}

/// Whether the name at `at` equals `name`, ignoring ASCII case.
fn name_equals(m: &[u8], mut at: usize, name: &str) -> Result<bool, DnsError> {
    let name = name.strip_suffix('.').unwrap_or(name);
    let mut labels = name.split('.');
    let mut jumps = 0;
    loop {
        match (next_label(m, &mut at, &mut jumps)?, labels.next()) {
            (None, None) => return Ok(true),
            (Some(got), Some(want)) if want.as_bytes().eq_ignore_ascii_case(got) => {}
            _ => return Ok(false),
        }
    }
}

/// Whether the names at offsets `a` and `b` are equal, ignoring ASCII case.
fn names_equal(m: &[u8], mut a: usize, mut b: usize) -> Result<bool, DnsError> {
    let (mut jumps_a, mut jumps_b) = (0, 0);
    loop {
        match (
            next_label(m, &mut a, &mut jumps_a)?,
            next_label(m, &mut b, &mut jumps_b)?,
        ) {
            (None, None) => return Ok(true),
            (Some(x), Some(y)) if x.eq_ignore_ascii_case(y) => {}
            _ => return Ok(false),
        }
    }
}

/// Parse a response to the A query for `name` sent with `id`.
pub fn parse_response(m: &[u8], id: u16, name: &str) -> Result<Answers, DnsError> {
    if m.len() < 12 {
        return Err(DnsError::Truncated);
    }
    if u16_at(m, 0)? != id {
        return Err(DnsError::IdMismatch);
    }
    let flags = u16_at(m, 2)?;
    if flags & 0x8000 == 0 {
        return Err(DnsError::NotAResponse);
    }
    if flags & 0x0200 != 0 {
        return Err(DnsError::TruncatedByServer);
    }
    if flags & 0x000f != 0 {
        return Err(DnsError::Rcode((flags & 0x000f) as u8));
    }
    if u16_at(m, 4)? != 1 || !name_equals(m, 12, name)? {
        return Err(DnsError::QuestionMismatch);
    }
    let mut at = skip_name(m, 12)?;
    if u16_at(m, at)? != TYPE_A || u16_at(m, at + 2)? != CLASS_IN {
        return Err(DnsError::QuestionMismatch);
    }
    at += 4;
    let mut answers = Answers {
        addrs: [Ipv4Addr::UNSPECIFIED; ADDRS_MAX],
        len: 0,
    };
    // Offset of the name whose addresses we want: the question name, then
    // each CNAME target in turn.
    let mut canonical = 12;
    for _ in 0..u16_at(m, 6)? {
        let owner = at;
        at = skip_name(m, at)?;
        let (rtype, class, rdlen) = (
            u16_at(m, at)?,
            u16_at(m, at + 2)?,
            u16_at(m, at + 8)? as usize,
        );
        at += 10; // type, class, ttl (ignored: we refresh on our own interval), rdlength
        let rdata = m.get(at..at + rdlen).ok_or(DnsError::Truncated)?;
        if class == CLASS_IN
            && (rtype == TYPE_A || rtype == TYPE_CNAME)
            && names_equal(m, owner, canonical)?
        {
            if rtype == TYPE_CNAME {
                canonical = at;
            } else if rdlen == 4 && answers.len < ADDRS_MAX {
                answers.addrs[answers.len] = Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]);
                answers.len += 1;
            }
        }
        at += rdlen;
    }
    if answers.len == 0 {
        return Err(DnsError::NoAddresses);
    }
    Ok(answers)
}

/// Nameserver addresses from resolv.conf text, IPv4 only, in file order.
pub fn parse_resolv_conf(text: &str) -> Vec<Ipv4Addr> {
    text.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("nameserver"))
                .then(|| w.next()?.parse().ok())
                .flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests;
