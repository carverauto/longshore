// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Optional headers for a store: content type, user metadata, storage class
//! and anything else the store understands, such as server-side encryption.
//!
//! An [`Options`] value has a fixed capacity and is `Copy`. It is built
//! before the request and each method fails if the addition does not fit or
//! is not allowed, so nothing is found out halfway through a request. The
//! operation keeps a copy, and every attempt sends and signs the same headers.

/// Entries one value holds.
pub const OPTIONS_MAX: usize = 8;
/// Bytes of names and values one value holds in total.
pub const OPTIONS_BYTES: usize = 512;
/// The longest name, so that a prefixed name always fits where it is built.
pub(crate) const NAME_MAX: usize = 96;

/// Headers the client sets itself. An option may not replace them.
const RESERVED: [&str; 14] = [
    "authorization",
    "connection",
    "content-length",
    "host",
    "if-none-match",
    "range",
    "transfer-encoding",
    "x-amz-checksum-crc64nvme",
    "x-amz-content-sha256",
    "x-amz-copy-source",
    "x-amz-date",
    "x-amz-security-token",
    "x-goog-hash",
    "x-goog-if-generation-match",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Naming {
    /// The name is sent as stored.
    Plain,
    /// The stored name follows the vendor's prefix: `x-amz-` or `x-goog-`.
    Vendor,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Entry {
    naming: Naming,
    name: (u16, u16),
    value: (u16, u16),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    bytes: [u8; OPTIONS_BYTES],
    used: u16,
    entries: [Entry; OPTIONS_MAX],
    count: u8,
}

impl Default for Options {
    fn default() -> Options {
        Options::new()
    }
}

impl Options {
    pub const fn new() -> Options {
        let none = Entry {
            naming: Naming::Plain,
            name: (0, 0),
            value: (0, 0),
        };
        Options {
            bytes: [0; OPTIONS_BYTES],
            used: 0,
            entries: [none; OPTIONS_MAX],
            count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn push(mut self, naming: Naming, parts: &[&str], value: &str) -> Option<Options> {
        let name_len: usize = parts.iter().map(|p| p.len()).sum();
        let token = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
        let name_ok =
            name_len > 0 && name_len <= NAME_MAX - 7 && parts.iter().all(|p| p.bytes().all(token));
        // A value is visible ASCII, spaces and tabs: no line breaks.
        let value_ok = value
            .bytes()
            .all(|b| b == b'\t' || (b' '..=b'~').contains(&b));
        let start = self.used as usize;
        let end = start + name_len + value.len();
        if !name_ok || !value_ok || self.count as usize == OPTIONS_MAX || end > OPTIONS_BYTES {
            return None;
        }
        let mut at = start;
        for part in parts {
            self.bytes[at..at + part.len()].copy_from_slice(part.as_bytes());
            at += part.len();
        }
        self.bytes[start..at].make_ascii_lowercase();
        self.bytes[at..end].copy_from_slice(value.as_bytes());
        let name = &self.bytes[start..at];
        let taken = |this: &Options| {
            (0..this.count as usize).any(|k| {
                let e = this.entries[k];
                e.naming == naming && this.slice(e.name) == name
            })
        };
        let reserved = naming == Naming::Plain && RESERVED.iter().any(|r| r.as_bytes() == name);
        if reserved || taken(&self) {
            return None;
        }
        self.entries[self.count as usize] = Entry {
            naming,
            name: (start as u16, name_len as u16),
            value: (at as u16, value.len() as u16),
        };
        self.count += 1;
        self.used = end as u16;
        Some(self)
    }

    fn slice(&self, range: (u16, u16)) -> &[u8] {
        &self.bytes[range.0 as usize..(range.0 + range.1) as usize]
    }

    /// The object's `Content-Type`.
    pub fn content_type(self, value: &str) -> Option<Options> {
        self.push(Naming::Plain, &["content-type"], value)
    }

    /// One entry of user metadata. The store returns it with the object, as
    /// `x-amz-meta-NAME` on S3 and `x-goog-meta-NAME` on Google Cloud Storage.
    pub fn metadata(self, name: &str, value: &str) -> Option<Options> {
        self.push(Naming::Vendor, &["meta-", name], value)
    }

    /// The storage class, in the store's own vocabulary.
    pub fn storage_class(self, value: &str) -> Option<Options> {
        self.push(Naming::Vendor, &["storage-class"], value)
    }

    /// Any other request header, sent as given: server-side encryption,
    /// `cache-control`, and so on. Headers the client sets itself are refused.
    pub fn header(self, name: &str, value: &str) -> Option<Options> {
        self.push(Naming::Plain, &[name], value)
    }

    /// Each header as it goes on the wire: the vendor's prefix for names
    /// that take one, the name, and the value.
    pub(crate) fn headers<'a>(
        &'a self,
        vendor_prefix: &'static str,
    ) -> impl Iterator<Item = (&'static str, &'a str, &'a str)> {
        self.entries[..self.count as usize].iter().map(move |e| {
            let text = |range| core::str::from_utf8(self.slice(range)).expect("checked when added");
            let prefix = match e.naming {
                Naming::Plain => "",
                Naming::Vendor => vendor_prefix,
            };
            (prefix, text(e.name), text(e.value))
        })
    }
}

#[cfg(test)]
mod tests;
