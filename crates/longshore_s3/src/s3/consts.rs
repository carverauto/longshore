// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Sizes of a slot's fixed buffers, and the header names the store reads.

use crate::multipart::UPLOAD_ID_MAX;
use crate::report::VERSION_ID_MAX;

/// Longest key once percent-encoded, with the slash before it.
pub const KEY_ENCODED_MAX: usize = 512;
/// A bucket name's longest length.
pub const BUCKET_MAX: usize = 63;
/// A request path: the bucket when it is addressed there, then the key.
pub(super) const PATH_MAX: usize = 1 + BUCKET_MAX + KEY_ENCODED_MAX;
/// The longest continuation token a listing can be resumed with, once
/// percent-encoded.
pub const LIST_TOKEN_ENCODED_MAX: usize = 1024;
/// A slot's request target: a path, or a listing's query with a token.
pub(super) const TARGET_MAX: usize =
    PATH_MAX + b"continuation-token=&".len() + LIST_TOKEN_ENCODED_MAX;
/// Room for a slot's query: `partNumber=10000&uploadId=` and an upload id,
/// or `versionId=` and a version id, with every byte percent-encoded.
pub(super) const QUERY_MAX: usize = {
    let multipart = 32 + 3 * UPLOAD_ID_MAX;
    let version = b"versionId=".len() + 3 * VERSION_ID_MAX;
    if multipart > version {
        multipart
    } else {
        version
    }
};
/// A credentials file must be smaller than this and than the HTTP buffer
/// size; a larger one is refused rather than read in part.
pub const CREDENTIALS_FILE_MAX: u32 = 8192;

pub(super) const CHECKSUM_HEADER: &str = "x-amz-checksum-crc64nvme";
