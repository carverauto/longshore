// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The multipart check: an object of three parts, each one buffer, stored in
//! parallel, completed, read back by range and erased; then an upload that
//! is aborted.

use super::*;
use crate::die::OrDie;
use longshore::store::multipart::{write_manifest, Part, UploadId};

const PARTS: u32 = 3;

fn fill(io: &mut UringIo, buf: BufId, size: u32, part: u32) {
    for (i, b) in io.buf(buf)[..size as usize].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[2] ^ part as u8;
    }
}

/// The ring, the client and the completions every step uses.
struct Ring {
    io: UringIo,
    s3: S3,
    out: Completions,
}

impl Ring {
    fn wait(&mut self) -> Done {
        wait(&mut self.io, &mut self.s3, &mut self.out)
    }
}

/// What each upload sends: the parts' checksums if `sums`, and the
/// checksum each part is stored with.
#[derive(Clone, Copy)]
struct Sums {
    sums: bool,
    checksum: Checksum,
}

fn begin(r: &mut Ring, key: &str, sums: bool) -> UploadId {
    r.s3.multipart_begin(&mut r.io, 0, key, sums)
        .or_die("multipart_begin");
    match r.wait().outcome {
        Outcome::Begun { upload } => upload,
        other @ (Outcome::Stored
        | Outcome::AlreadyExists { .. }
        | Outcome::Fetched { .. }
        | Outcome::Listed { .. }
        | Outcome::NotFound
        | Outcome::Erased
        | Outcome::PartStored { .. }
        | Outcome::Aborted
        | Outcome::Stat(_)
        | Outcome::Copied
        | Outcome::ErasedMany { .. }
        | Outcome::Report(_)
        | Outcome::Fatal(_)) => die(format_args!("multipart begin -> {other:?}")),
    }
}

/// The parts that were stored, from `multipart_store_part` on each of
/// `bufs`, all in flight at once, and the attempts they took in total.
fn store_parts(
    r: &mut Ring,
    (key, upload): (&str, &UploadId),
    bufs: &[BufId],
    size: u32,
    checksum: Checksum,
) -> ([Part; PARTS as usize], u32) {
    for part in 1..=PARTS {
        let buf = bufs[part as usize - 1];
        r.s3.multipart_store_part(
            &mut r.io,
            u64::from(part),
            key,
            upload,
            part,
            buf,
            size,
            checksum,
        )
        .or_die("multipart_store_part");
    }
    let mut parts: [Option<Part>; PARTS as usize] = [None; PARTS as usize];
    let mut attempts = 0;
    for _ in 0..PARTS {
        let d = r.wait();
        attempts += d.attempts;
        match d.outcome {
            Outcome::PartStored { tag, checksum: crc } => {
                parts[d.token as usize - 1] = Some(Part {
                    number: d.token as u32,
                    tag,
                    checksum: crc.map(longshore::checksum::crc64_header_value),
                })
            }
            other @ (Outcome::Stored
            | Outcome::AlreadyExists { .. }
            | Outcome::Fetched { .. }
            | Outcome::Listed { .. }
            | Outcome::NotFound
            | Outcome::Erased
            | Outcome::Begun { .. }
            | Outcome::Aborted
            | Outcome::Stat(_)
            | Outcome::Copied
            | Outcome::ErasedMany { .. }
            | Outcome::Report(_)
            | Outcome::Fatal(_)) => die(format_args!("part {} -> {other:?}", d.token)),
        }
    }
    (parts.map(Option::unwrap), attempts)
}

/// Begin an upload, store the parts in parallel and complete it, with
/// `object_sum` as the whole object's checksum if given. The manifest is
/// written to the spare buffer after the parts'. Returns what completion
/// did and the attempts the parts took.
fn upload(
    r: &mut Ring,
    key: &str,
    bufs: &[BufId],
    size: u32,
    sums: Sums,
    object_sum: Option<u64>,
) -> (Done, u32) {
    let upload = begin(r, key, sums.sums);
    let (parts, attempts) = store_parts(r, (key, &upload), bufs, size, sums.checksum);
    let manifest = bufs[PARTS as usize];
    let manifest_len = write_manifest(r.io.buf(manifest), &parts).or_die("write_manifest") as u32;
    r.s3.multipart_complete(
        &mut r.io,
        10,
        key,
        &upload,
        manifest,
        manifest_len,
        object_sum,
    )
    .or_die("multipart_complete");
    (r.wait(), attempts)
}

/// Read each part back by range into the spare buffer and compare it with
/// the buffer it was stored from.
fn read_back(r: &mut Ring, key: &str, bufs: &[BufId], size: u32) -> bool {
    let spare = bufs[PARTS as usize];
    let mut read_ok = true;
    for part in 1..=PARTS {
        let from = (u64::from(part) - 1) * u64::from(size);
        r.s3.fetch(
            &mut r.io,
            20,
            key,
            Some((from, from + u64::from(size) - 1)),
            spare,
        )
        .or_die("fetch");
        let d = r.wait();
        read_ok &= matches!(d.outcome, Outcome::Fetched { len, .. } if len == size);
        let (a, b) = (bufs[part as usize - 1], spare);
        let sent = checksum_of(&r.io.buf(a)[..size as usize]);
        read_ok &= sent == checksum_of(&r.io.buf(b)[..size as usize]);
    }
    read_ok
}

/// An upload that is abandoned: begin, one part from `buf`, abort.
fn abandon(r: &mut Ring, key: &str, buf: BufId, size: u32, sums: Sums) -> Done {
    let abandoned = begin(r, key, sums.sums);
    r.s3.multipart_store_part(&mut r.io, 1, key, &abandoned, 1, buf, size, sums.checksum)
        .or_die("multipart_store_part");
    let part = r.wait();
    if !matches!(part.outcome, Outcome::PartStored { .. }) {
        die(format_args!("part before abort -> {:?}", part.outcome));
    }
    r.s3.multipart_abort(&mut r.io, 40, key, &abandoned)
        .or_die("multipart_abort");
    r.wait()
}

/// `size` is the size of each part. Stores refuse parts under 5 MiB other
/// than the last, so the default of 8 MiB is a sensible one.
pub(super) fn multipart(size: u32) {
    // S3_MULTIPART_CHECKSUMS=0 for stores without whole-object CRC-64/NVME.
    let sums = !std::env::var("S3_MULTIPART_CHECKSUMS").is_ok_and(|v| v == "0");
    let gcs = std::env::var("S3_VENDOR").is_ok_and(|v| v == "gcs");
    let (io, s3, first) = setup(PARTS, PARTS + 1, size, PARTS + 1);
    let mut r = Ring {
        io,
        s3,
        out: Completions::new(32),
    };
    let bufs: Vec<BufId> = (0..PARTS + 1).map(|i| BufId(first + i)).collect();
    let key = format!(
        "selftest/multipart-{}-{}.bin",
        std::process::id(),
        r.io.unix_secs()
    );
    let mut whole = longshore::checksum::Crc64Nvme::new();
    for part in 1..=PARTS {
        fill(&mut r.io, bufs[part as usize - 1], size, part);
        whole.update(&r.io.buf(bufs[part as usize - 1])[..size as usize]);
    }
    let checksum = if sums {
        Checksum::Compute
    } else {
        Checksum::None
    };
    let both = Sums { sums, checksum };
    let t0 = r.io.now_ns();

    // Sealed: begin, the parts in parallel, completion, read-back, erase, abort.
    let (stored, read_ok, erased, aborted, attempts) = sealed(|| {
        // Google Cloud Storage keeps CRC-32C, so there is no CRC-64 to send it.
        let object_sum = (sums && !gcs).then(|| whole.finish());
        let (stored, attempts) = upload(&mut r, &key, &bufs, size, both, object_sum);
        let read_ok = read_back(&mut r, &key, &bufs, size);
        r.s3.erase(&mut r.io, 30, &key).or_die("erase");
        let erased = r.wait();
        let aborted = abandon(&mut r, &key, bufs[0], size, both);
        (stored, read_ok, erased, aborted, attempts)
    });
    let ms = (r.io.now_ns() - t0) / MS;

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
        r.s3.connects()
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
