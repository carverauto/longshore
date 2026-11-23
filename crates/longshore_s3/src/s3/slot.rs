// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One operation in the store's fixed table.

use super::consts::{QUERY_MAX, TARGET_MAX};
use super::exact;
use super::kind::Kind;
use super::state::State;
use super::Outcome;
use crate::options::Options;
use longshore_common::io::BufId;

#[derive(Clone, Copy)]
pub(super) struct Slot {
    pub(super) state: State,
    /// The result, once `state` is `Finished`.
    pub(super) outcome: Outcome,
    pub(super) token: u64,
    pub(super) kind: Kind,
    pub(super) path: [u8; TARGET_MAX],
    pub(super) path_len: usize,
    /// Encoded query of a multipart request, in the sorted order signing
    /// requires. For a copy, the source as its header carries it. Empty
    /// for every other kind.
    pub(super) query: [u8; QUERY_MAX],
    pub(super) query_len: usize,
    /// Optional headers of a store or of beginning an upload.
    pub(super) options: Options,
    pub(super) body: Option<(BufId, u32)>,
    pub(super) response_buf: Option<BufId>,
    pub(super) attempts: u32,
    pub(super) delays: u32,
    /// Times this operation waited for an address without starting an attempt.
    pub(super) address_waits: u32,
    /// Sign with the candidate credentials (the validation request only).
    pub(super) candidate: bool,
    /// A retried conditional store was refused as already existing: this
    /// attempt asks for the stored object's checksum to see whether an
    /// earlier attempt of the same operation is what created it.
    pub(super) comparing: bool,
    /// CRC-64/NVME of the body, computed once and reused by every attempt.
    pub(super) crc: Option<u64>,
    /// The caller's SHA-256 of the body, sent instead of a CRC-64/NVME.
    pub(super) sha256: Option<[u8; 32]>,
    /// Set for `put`, `head` and `get`, which end with a [`Report`](crate::report::Report).
    pub(super) exact: Option<exact::Exact>,
}
