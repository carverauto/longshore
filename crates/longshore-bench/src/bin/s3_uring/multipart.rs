// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The multipart check: an object of three parts, each one buffer, stored in
//! parallel, completed, read back by range and erased; then an upload that
//! is aborted.

use super::*;
use longshore::store::multipart::{write_manifest, Part, UploadId};

const PARTS: u32 = 3;

fn fill(io: &mut UringIo, buf: BufId, size: u32, part: u32) {
    for (i, b) in io.buf(buf)[..size as usize].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[2] ^ part as u8;
    }
}

fn begin(io: &mut UringIo, s3: &mut S3, out: &mut Completions, key: &str, sums: bool) -> UploadId {
    s3.multipart_begin(io, 0, key, sums).unwrap();
    match wait(io, s3, out).outcome {
        Outcome::Begun { upload } => upload,
        other => die(&format!("multipart begin -> {other:?}")),
    }
}

/// `size` is the size of each part. Stores refuse parts under 5 MiB other
/// than the last, so the default of 8 MiB is a sensible one.
pub(super) fn multipart(size: u32) {
    // S3_MULTIPART_CHECKSUMS=0 for stores without whole-object CRC-64/NVME.
    let sums = !std::env::var("S3_MULTIPART_CHECKSUMS").is_ok_and(|v| v == "0");
    let gcs = std::env::var("S3_VENDOR").is_ok_and(|v| v == "gcs");
    let (mut io, mut s3, first) = setup(PARTS, PARTS + 1, size, PARTS + 1);
    let bufs: Vec<BufId> = (0..PARTS + 1).map(|i| BufId(first + i)).collect();
    let key = format!(
        "selftest/multipart-{}-{}.bin",
        std::process::id(),
        io.unix_secs()
    );
    let mut out = Completions::new(32);
    let mut whole = longshore::checksum::Crc64Nvme::new();
    for part in 1..=PARTS {
        fill(&mut io, bufs[part as usize - 1], size, part);
        whole.update(&io.buf(bufs[part as usize - 1])[..size as usize]);
    }
    let checksum = if sums {
        Checksum::Compute
    } else {
        Checksum::None
    };
    let t0 = io.now_ns();

    // Sealed: begin, the parts in parallel, completion, read-back, erase, abort.
    let (stored, read_ok, erased, aborted, attempts) = sealed(|| {
        let upload = begin(&mut io, &mut s3, &mut out, &key, sums);
        for part in 1..=PARTS {
            let buf = bufs[part as usize - 1];
            s3.multipart_store_part(
                &mut io,
                part as u64,
                &key,
                &upload,
                part,
                buf,
                size,
                checksum,
            )
            .unwrap();
        }
        let mut parts: [Option<Part>; PARTS as usize] = [None; PARTS as usize];
        let mut attempts = 0;
        for _ in 0..PARTS {
            let d = wait(&mut io, &mut s3, &mut out);
            attempts += d.attempts;
            match d.outcome {
                Outcome::PartStored { tag, checksum } => {
                    parts[d.token as usize - 1] = Some(Part {
                        number: d.token as u32,
                        tag,
                        checksum: checksum.map(longshore::checksum::crc64_header_value),
                    })
                }
                other => die(&format!("part {} -> {other:?}", d.token)),
            }
        }
        let parts = parts.map(Option::unwrap);
        let manifest = bufs[PARTS as usize];
        let len = write_manifest(io.buf(manifest), &parts).unwrap() as u32;
        // Google Cloud Storage keeps CRC-32C, so there is no CRC-64 to send it.
        let object_sum = (sums && !gcs).then(|| whole.finish());
        s3.multipart_complete(&mut io, 10, &key, &upload, manifest, len, object_sum)
            .unwrap();
        let stored = wait(&mut io, &mut s3, &mut out);

        // Read each part back by range into the spare buffer and compare.
        let mut read_ok = true;
        for part in 1..=PARTS {
            let from = (part as u64 - 1) * size as u64;
            s3.fetch(
                &mut io,
                20,
                &key,
                Some((from, from + size as u64 - 1)),
                manifest,
            )
            .unwrap();
            let d = wait(&mut io, &mut s3, &mut out);
            read_ok &= matches!(d.outcome, Outcome::Fetched { len, .. } if len == size);
            let (a, b) = (bufs[part as usize - 1], manifest);
            let sent = checksum_of(&io.buf(a)[..size as usize]);
            read_ok &= sent == checksum_of(&io.buf(b)[..size as usize]);
        }
        s3.erase(&mut io, 30, &key).unwrap();
        let erased = wait(&mut io, &mut s3, &mut out);

        // An upload that is abandoned: begin, one part, abort.
        let upload = begin(&mut io, &mut s3, &mut out, &key, sums);
        s3.multipart_store_part(&mut io, 1, &key, &upload, 1, bufs[0], size, checksum)
            .unwrap();
        let part = wait(&mut io, &mut s3, &mut out);
        if !matches!(part.outcome, Outcome::PartStored { .. }) {
            die(&format!("part before abort -> {:?}", part.outcome));
        }
        s3.multipart_abort(&mut io, 40, &key, &upload).unwrap();
        let aborted = wait(&mut io, &mut s3, &mut out);
        (stored, read_ok, erased, aborted, attempts)
    });
    let ms = (io.now_ns() - t0) / MS;

    println!("parts={PARTS} part_bytes={size} part_checksums={sums} part_attempts={attempts}");
    println!(
        "complete                   -> {:?} (attempts {})",
        stored.outcome, stored.attempts
    );
    println!("erase                      -> {:?}", erased.outcome);
    println!("abort                      -> {:?}", aborted.outcome);
    let ok = stored.outcome == Outcome::Stored
        && read_ok
        && erased.outcome == Outcome::Erased
        && aborted.outcome == Outcome::Aborted;
    println!(
        "bytes_match={read_ok} connections={} elapsed_ms={ms} allocation_sealed=true",
        s3.connects()
    );
    println!(
        "{}",
        if ok {
            "multipart ok"
        } else {
            "multipart FAILED"
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}
