// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a store reports about an object without sending it.

use crate::store::multipart::PartTag;

/// The result of `S3::stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// The object's length in bytes.
    pub size: u64,
    /// When it was last written, in Unix seconds, if the store said so in a
    /// form this client reads.
    pub modified: Option<u64>,
    /// The store's checksum of the object as its header carries it, padded
    /// with zeros: twelve base64 characters of CRC-64/NVME on S3, eight of
    /// CRC-32C on Google Cloud Storage.
    pub checksum: Option<[u8; 12]>,
    /// The object's `ETag`, quotes included, if it fits.
    pub tag: Option<PartTag>,
    /// With a buffer given to `stat`: how many bytes of the response head
    /// were copied into it. Read headers from them with [`header_in`].
    pub head_len: u32,
}

/// The value of header `name` in a response head, as `stat` copies it into
/// the caller's buffer. Names compare without regard to case. Use it for
/// what [`Stat`] has no field for: `content-type`, `x-amz-meta-...`,
/// `x-amz-storage-class` and so on.
pub fn header_in<'a>(head: &'a [u8], name: &str) -> Option<&'a [u8]> {
    // The first line is the status line.
    head.split(|b| *b == b'\n').skip(1).find_map(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let colon = line.iter().position(|b| *b == b':')?;
        line[..colon]
            .eq_ignore_ascii_case(name.as_bytes())
            .then(|| line[colon + 1..].trim_ascii())
    })
}

#[cfg(test)]
mod tests;
