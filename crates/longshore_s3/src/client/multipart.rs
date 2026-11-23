// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart upload through the shareable client, for an object larger
//! than one lent buffer.
//!
//! The caller keeps the upload's state: the [`UploadId`] that
//! [`Client::upload_begin`] returns and the [`Part`] each
//! [`Client::upload_part`] returns. Parts are independent operations, so
//! they can be in flight together and on different I/O threads. To finish,
//! [`Client::upload_complete`] sends the parts in order. An upload that is
//! neither completed nor aborted keeps its parts in the store.

use super::lane::{Finished, Kind};
use super::{error_of, Buffer, Client, Error, Pending};
use crate::multipart::{write_manifest, Part, UploadId};
use crate::s3::{Checksum, Outcome};

impl Client {
    fn sends_checksums(&self) -> bool {
        self.checksum == Checksum::Compute
    }

    /// Begin a multipart upload to `key`. Keep the id: every later call
    /// takes it.
    ///
    /// If this handle sends checksums (see [`Client::with_checksums`]), the
    /// store is asked to keep a CRC-64/NVME of the whole object, and every
    /// part must then be stored with a checksum too. So begin an upload and
    /// store its parts through handles with the same setting. For a store
    /// that keeps no checksums, use a handle with them off: a store may
    /// refuse a completion that lists checksums it did not keep.
    ///
    /// If the answer to this call is lost the request is sent again, and
    /// the first upload is left behind unused.
    pub fn upload_begin(&self, key: &str) -> Pending<Result<UploadId, Error>> {
        let kind = Kind::Begin {
            checksums: self.sends_checksums(),
        };
        self.unbuffered(key, kind, |f, _| match f {
            Finished::Outcome(Outcome::Begun { upload }) => Ok(*upload),
            other @ (Finished::Outcome(_) | Finished::Rejected(_) | Finished::Closed) => {
                Err(error_of(other))
            }
        })
    }

    /// Store the first `len` bytes of `body` as part `number` (1 to 10000)
    /// of an upload. Storing a number again replaces that part. Every part
    /// but the last must be at least as large as the store requires, 5 MiB
    /// on Amazon S3. Keep the [`Part`]: completing the upload needs it.
    pub fn upload_part(
        &self,
        key: &str,
        upload: &UploadId,
        number: u32,
        body: Buffer,
        len: usize,
    ) -> Pending<(Result<Part, Error>, Buffer)> {
        let too_long = len > body.lane.buf_size;
        let (lane, buf, alive) = body.into_parts();
        let kind = Kind::Part {
            checksum: self.checksum,
        };
        let early = too_long.then_some(Error::TooLong);
        Pending::new(
            (lane, alive),
            key,
            kind,
            Some(buf),
            len as u32,
            early,
            |f, part_number| match f {
                Finished::Outcome(Outcome::PartStored { tag, checksum }) => Ok(Part {
                    number: part_number,
                    tag: *tag,
                    checksum: checksum.map(longshore_crc::crc64_header_value),
                }),
                other @ (Finished::Outcome(_) | Finished::Rejected(_) | Finished::Closed) => {
                    Err(error_of(other))
                }
            },
        )
        .of_upload(upload, number)
    }

    /// Complete an upload from its parts, which must be in ascending order
    /// of number. The list of parts is written into `scratch`, a lent
    /// buffer whose contents are replaced, and which comes back with the
    /// result. [`Error::TooLong`] if the list does not fit in it, and
    /// [`Error::BadRange`] if the parts are not in order.
    ///
    /// If the answer to this call is lost, the request is sent again and
    /// the store may then say the upload is gone, which is reported as an
    /// error although the object is there.
    pub fn upload_complete(
        &self,
        key: &str,
        upload: &UploadId,
        parts: &[Part],
        mut scratch: Buffer,
    ) -> Pending<(Result<(), Error>, Buffer)> {
        let ordered = parts.windows(2).all(|p| p[0].number < p[1].number);
        let written = write_manifest(scratch.as_mut_slice(), parts);
        let early = match (ordered, written) {
            (false, _) => Some(Error::BadRange),
            (true, None) => Some(Error::TooLong),
            (true, Some(_)) => None,
        };
        let (lane, buf, alive) = scratch.into_parts();
        Pending::new(
            (lane, alive),
            key,
            Kind::Complete,
            Some(buf),
            written.unwrap_or(0) as u32,
            early,
            |f, _| match f {
                Finished::Outcome(Outcome::Stored) => Ok(()),
                other @ (Finished::Outcome(_) | Finished::Rejected(_) | Finished::Closed) => {
                    Err(error_of(other))
                }
            },
        )
        .of_upload(upload, 0)
    }

    /// Abort an upload and discard its parts. Aborting an upload that no
    /// longer exists succeeds.
    pub fn upload_abort(&self, key: &str, upload: &UploadId) -> Pending<Result<(), Error>> {
        self.unbuffered(key, Kind::Abort, |f, _| match f {
            Finished::Outcome(Outcome::Aborted) => Ok(()),
            other @ (Finished::Outcome(_) | Finished::Rejected(_) | Finished::Closed) => {
                Err(error_of(other))
            }
        })
        .of_upload(upload, 0)
    }
}
