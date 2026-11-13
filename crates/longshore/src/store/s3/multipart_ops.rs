// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart upload: begin, store parts, complete or abort.
//!
//! An upload removes the limit of one buffer per object. The caller keeps
//! the [`UploadId`] from [`Outcome::Begun`] and the [`PartTag`] from each
//! [`Outcome::PartStored`], writes the list of parts into a registered buffer
//! with [`multipart::write_manifest`], and completes. Parts may be stored
//! concurrently and in any order.
//!
//! An upload that is neither completed nor aborted keeps its parts in the
//! store. A bucket rule that expires incomplete uploads is the backstop.

use super::*;

impl S3 {
    /// `name=` followed by the encoded upload id, after `prefix`.
    fn upload_query(prefix: &[u8], upload: &UploadId, out: &mut [u8; QUERY_MAX]) -> usize {
        let mut n = prefix.len();
        out[..n].copy_from_slice(prefix);
        out[n..n + 9].copy_from_slice(b"uploadId=");
        n += 9;
        n + uri_encode(upload.as_bytes(), true, &mut out[n..])
            .expect("the query buffer holds any upload id fully encoded")
    }

    /// Begin a multipart upload to `key`. The outcome is [`Outcome::Begun`].
    ///
    /// With `checksums`, the store is asked to keep a whole-object
    /// CRC-64/NVME, which each part and the completion can then carry. Leave
    /// it off for stores that do not support that.
    ///
    /// If the response to this call is lost the operation is sent again, and
    /// the first upload is left behind unused.
    pub fn multipart_begin<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        checksums: bool,
    ) -> Result<(), SubmitError> {
        // Google Cloud Storage takes checksums per part without being asked.
        let checksums = checksums && self.cfg.vendor == Vendor::S3;
        self.submit(
            io,
            token,
            key,
            Kind::Begin { checksums },
            None,
            None,
            Checksum::None,
            b"uploads=",
        )
    }

    /// Store `len` bytes of `body` as part `number` (1 to 10000) of an
    /// upload. The outcome is [`Outcome::PartStored`]. Storing the same
    /// number again replaces the part.
    #[allow(clippy::too_many_arguments)]
    pub fn multipart_store_part<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        upload: &UploadId,
        number: u32,
        body: BufId,
        len: u32,
        checksum: Checksum,
    ) -> Result<(), SubmitError> {
        if number == 0 || number > PART_NUMBER_MAX {
            return Err(SubmitError::BadPart);
        }
        let mut prefix = *b"partNumber=00000&";
        let mut at = 16;
        let mut v = number;
        while v > 0 {
            at -= 1;
            prefix[at] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        // Leading zeros are dropped: "partNumber=" then the digits.
        let digits = 16 - at;
        prefix.copy_within(at..17, 11);
        let mut query = [0u8; QUERY_MAX];
        let n = Self::upload_query(&prefix[..11 + digits + 1], upload, &mut query);
        self.submit(
            io,
            token,
            key,
            Kind::Part,
            Some((body, len)),
            None,
            checksum,
            &query[..n],
        )
    }

    /// Complete an upload. `manifest` holds `len` bytes written by
    /// [`multipart::write_manifest`]. The outcome is [`Outcome::Stored`].
    ///
    /// `object_checksum` is the checksum of the whole object, CRC-64/NVME on
    /// S3. With it the store verifies the assembled object, and a completion
    /// whose response was lost can be confirmed instead of reported as a
    /// failure. It needs an upload begun with `checksums`.
    #[allow(clippy::too_many_arguments)]
    pub fn multipart_complete<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        upload: &UploadId,
        manifest: BufId,
        len: u32,
        object_checksum: Option<u64>,
    ) -> Result<(), SubmitError> {
        let mut query = [0u8; QUERY_MAX];
        let n = Self::upload_query(b"", upload, &mut query);
        self.submit(
            io,
            token,
            key,
            Kind::Complete,
            Some((manifest, len)),
            None,
            object_checksum.map_or(Checksum::None, Checksum::Provided),
            &query[..n],
        )
    }

    /// Abort an upload and discard its parts. The outcome is
    /// [`Outcome::Aborted`], also when the upload no longer exists.
    pub fn multipart_abort<IO: Io>(
        &mut self,
        io: &mut IO,
        token: u64,
        key: &str,
        upload: &UploadId,
    ) -> Result<(), SubmitError> {
        let mut query = [0u8; QUERY_MAX];
        let n = Self::upload_query(b"", upload, &mut query);
        self.submit(
            io,
            token,
            key,
            Kind::Abort,
            None,
            None,
            Checksum::None,
            &query[..n],
        )
    }

    /// The outcome of a multipart request that the store answered with a
    /// success status, when that needs more than the status to decide.
    /// `None` leaves the response to the common handling.
    pub(super) fn multipart_outcome<IO: Io>(
        &mut self,
        io: &mut IO,
        kind: Kind,
        r: client::Response,
    ) -> Option<Result<Outcome, Class>> {
        match kind {
            Kind::Begin { .. } => {
                let doc = self.client.discarded_body(io, r.conn);
                let upload = multipart::upload_id_in(doc).and_then(UploadId::new);
                Some(Ok(upload
                    .map_or(Outcome::Fatal(Fatal::BadResponse), |upload| {
                        Outcome::Begun { upload }
                    })))
            }
            Kind::Part => {
                let tag = self
                    .client
                    .header(io, r.conn, "etag")
                    .and_then(PartTag::new);
                Some(Ok(tag.map_or(Outcome::Fatal(Fatal::BadResponse), |tag| {
                    Outcome::PartStored { tag }
                })))
            }
            Kind::Complete => {
                // The store may take long enough to assemble the object that
                // it has already sent 200 when it fails: the body says which.
                let doc = self.client.discarded_body(io, r.conn);
                if doc.is_empty() && r.body_len > 0 {
                    return Some(Ok(Outcome::Fatal(Fatal::BadResponse)));
                }
                let code = multipart::error_code_in(doc)?;
                const TRANSIENT: [&[u8]; 4] = [
                    b"InternalError",
                    b"SlowDown",
                    b"ServiceUnavailable",
                    b"RequestTimeout",
                ];
                Some(if TRANSIENT.contains(&code) {
                    Err(Class::RetryDelayed)
                } else {
                    Ok(Outcome::Fatal(Fatal::ErrorDocument))
                })
            }
            _ => None,
        }
    }
}
