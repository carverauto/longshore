// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `client_s3 multipart`: objects larger than one buffer, uploaded in parts
//! through the shareable client against a real store, then read back.

use longshore::client::{Client, Error};
use std::time::Instant;

fn pattern(object: u32, part: u32, i: usize) -> u8 {
    (i as u32).wrapping_mul(2654435761).to_le_bytes()[2] ^ (object * 31 + part * 7) as u8
}

fn fail(what: &str, e: Error) -> ! {
    eprintln!("client_s3: {what}: {e:?}");
    std::process::exit(3);
}

/// Upload `objects` objects of `parts` parts of `size` bytes each, every
/// object's parts in flight together, complete each upload, and check
/// every part with a ranged fetch. Then begin one more upload, store a
/// part, abort it, and check that it can no longer be completed. Prints
/// one JSON line.
pub fn run(client: &Client, size: usize, parts: u32, objects: u32) {
    let pid = std::process::id();
    let started = Instant::now();
    for object in 0..objects {
        let key = format!("bench/{pid}-multipart-{object}.bin");
        let id = client
            .upload_begin(&key)
            .wait()
            .unwrap_or_else(|e| fail("begin", e));
        let sent: Vec<_> = (1..=parts)
            .map(|part| {
                let mut body = client.buffer_blocking();
                for (i, b) in body.as_mut_slice()[..size].iter_mut().enumerate() {
                    *b = pattern(object, part, i);
                }
                client.upload_part(&key, &id, part, body, size)
            })
            .collect();
        let mut stored = Vec::new();
        let mut scratch = None;
        for pending in sent {
            let (part, buffer) = pending.wait();
            stored.push(part.unwrap_or_else(|e| fail("store a part", e)));
            scratch = Some(buffer);
        }
        let scratch = scratch.expect("at least one part");
        let (completed, mut into) = client.upload_complete(&key, &id, &stored, scratch).wait();
        completed.unwrap_or_else(|e| fail("complete", e));
        for part in 1..=parts {
            let first = (u64::from(part) - 1) * size as u64;
            let (fetched, buffer) = client
                .fetch_range(&key, first, first + size as u64 - 1, into)
                .wait();
            match fetched {
                Ok(Some(n)) if n == size => {}
                Ok(other) => {
                    eprintln!("client_s3: part {part} of {key} read back as {other:?}");
                    std::process::exit(3);
                }
                Err(e) => fail("read a part back", e),
            }
            if let Some(at) = (0..size).find(|i| buffer.as_slice()[*i] != pattern(object, part, *i))
            {
                eprintln!("client_s3: part {part} of {key} differs at byte {at}");
                std::process::exit(3);
            }
            into = buffer;
        }
        drop(into);
        client
            .erase(&key)
            .wait()
            .unwrap_or_else(|e| fail("erase", e));
    }
    let secs = started.elapsed().as_secs_f64();

    let key = format!("bench/{pid}-multipart-aborted.bin");
    let id = client
        .upload_begin(&key)
        .wait()
        .unwrap_or_else(|e| fail("begin", e));
    let (part, scratch) = client
        .upload_part(&key, &id, 1, client.buffer_blocking(), size)
        .wait();
    let part = part.unwrap_or_else(|e| fail("store a part", e));
    client
        .upload_abort(&key, &id)
        .wait()
        .unwrap_or_else(|e| fail("abort", e));
    let (completed, _) = client.upload_complete(&key, &id, &[part], scratch).wait();
    let refused = matches!(completed, Err(Error::Store(_)));
    if !refused {
        eprintln!("client_s3: completing an aborted upload ended {completed:?}");
        std::process::exit(3);
    }
    println!(
        "{{\"cmd\":\"client-multipart\",\"arch\":\"{}\",\"threads\":{},\"objects\":{objects},\"parts\":{parts},\"size\":{size},\"object_bytes\":{},\"secs\":{secs:.2},\"verified_parts\":{},\"aborted_upload_refused\":{refused}}}",
        std::env::consts::ARCH,
        client.threads(),
        u64::from(parts) * size as u64,
        objects * parts,
    );
}
