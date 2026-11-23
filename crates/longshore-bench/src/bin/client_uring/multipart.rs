// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A multipart upload through the shareable client on real I/O threads.

use crate::die::OrDie;
use longshore::client::{Client, Error};

fn pattern(part: u32, i: usize) -> u8 {
    (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ (part * 37) as u8
}

/// Upload three parts of `part_len` bytes, two of them in flight together
/// and so on whichever I/O threads lent their buffers, complete the upload,
/// and read the object back. The three parts together must fit one buffer,
/// which is as much as the test server assembles.
pub fn round_trip(client: &Client, part_len: usize) {
    let key = "multipart-object";
    let id = client.upload_begin(key).wait().or_die("begin an upload");
    let filled = |part: u32| {
        let mut body = client.buffer_blocking();
        for (i, b) in body.as_mut_slice()[..part_len].iter_mut().enumerate() {
            *b = pattern(part, i);
        }
        body
    };
    let one = client.upload_part(key, &id, 1, filled(1), part_len);
    let two = client.upload_part(key, &id, 2, filled(2), part_len);
    let (one, _) = one.wait();
    let (two, _) = two.wait();
    let (three, scratch) = client.upload_part(key, &id, 3, filled(3), part_len).wait();
    let parts = [one, two, three].map(|p| p.or_die("store a part"));
    let (completed, into) = client.upload_complete(key, &id, &parts, scratch).wait();
    assert_eq!(completed, Ok(()), "complete the upload");

    let (fetched, into) = client.fetch(key, into).wait();
    assert_eq!(fetched, Ok(Some(3 * part_len)), "the assembled object");
    for part in 1..=3u32 {
        let at = (part as usize - 1) * part_len;
        assert!(
            into.as_slice()[at..at + part_len]
                .iter()
                .enumerate()
                .all(|(i, b)| *b == pattern(part, i)),
            "part {part} of the assembled object differs"
        );
    }
    drop(into);

    // An aborted upload is gone: completing it is refused by the store.
    let aborted = client
        .upload_begin("multipart-aborted")
        .wait()
        .or_die("begin");
    let (part, buffer) = client
        .upload_part("multipart-aborted", &aborted, 1, filled(1), part_len)
        .wait();
    let part = part.or_die("store a part");
    assert_eq!(
        client.upload_abort("multipart-aborted", &aborted).wait(),
        Ok(())
    );
    let (refused, _) = client
        .upload_complete("multipart-aborted", &aborted, &[part], buffer)
        .wait();
    assert!(matches!(refused, Err(Error::Store(_))), "{refused:?}");
    assert_eq!(client.erase(key).wait(), Ok(()));
}
