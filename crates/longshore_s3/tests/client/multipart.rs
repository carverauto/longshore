// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Multipart upload through the shareable client, against the in-tree
//! server in the simulator.

#[cfg(test)]
#[path = "../store/common/mod.rs"]
mod common;
#[cfg(test)]
mod rig;
use longshore_s3::client::Error;
use longshore_s3::multipart::{Part, PartTag};
use rig::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Begin an upload, store two parts that are in flight together, and
/// complete it. Returns the bytes the object should hold.
#[cfg(test)]
fn upload(r: &mut Rig, client: &longshore_s3::client::Client, key: &str) -> (Vec<u8>, Vec<Part>) {
    let id = r.run(client.upload_begin(key)).expect("begin");
    let (mut first, mut second) = (r.buffer(), r.buffer());
    fill(&mut first, 1);
    fill(&mut second, 2);
    let mut object = first.as_slice()[..LEN].to_vec();
    object.extend_from_slice(&second.as_slice()[..LEN - 7]);

    let mut cx = Context::from_waker(Waker::noop());
    let mut one = pin!(client.upload_part(key, &id, 1, first, LEN));
    let mut two = pin!(client.upload_part(key, &id, 2, second, LEN - 7));
    assert!(one.as_mut().poll(&mut cx).is_pending());
    assert!(two.as_mut().poll(&mut cx).is_pending());
    let (mut parts, mut scratch) = (Vec::new(), None);
    let (mut one_done, mut two_done) = (false, false);
    for _ in 0..100_000 {
        if one_done && two_done {
            break;
        }
        r.pump();
        if !one_done {
            if let Poll::Ready((part, buffer)) = one.as_mut().poll(&mut cx) {
                parts.push(part.expect("part 1"));
                scratch = Some(buffer);
                one_done = true;
            }
        }
        if !two_done {
            if let Poll::Ready((part, _buffer)) = two.as_mut().poll(&mut cx) {
                parts.push(part.expect("part 2"));
                two_done = true;
            }
        }
    }
    parts.sort_by_key(|p| p.number());
    assert_eq!(parts.len(), 2, "the parts did not end");
    let (completed, _scratch) = r.run(client.upload_complete(key, &id, &parts, scratch.unwrap()));
    assert_eq!(completed, Ok(()));
    (object, parts)
}

#[test]
fn parts_stored_together_become_one_object() {
    let mut r = rig(1);
    let client = r.client.clone();
    let (object, parts) = upload(&mut r, &client, "video/a");
    assert_eq!(r.lanes[0].server.object("/video/a"), Some(&object[..]));
    // Each part went with a checksum, and reports it for the completion.
    assert!(parts.iter().all(|p| p.checksum().is_some()));
    assert_eq!(parts[0].number(), 1);
    assert_eq!(parts[1].number(), 2);
}

#[test]
fn a_handle_with_checksums_off_uploads_without_them() {
    let mut r = rig(1);
    let client = r.client.clone().with_checksums(false);
    let (object, parts) = upload(&mut r, &client, "video/b");
    assert_eq!(r.lanes[0].server.object("/video/b"), Some(&object[..]));
    assert!(parts.iter().all(|p| p.checksum().is_none()));
}

#[test]
fn an_aborted_upload_cannot_be_completed() {
    let mut r = rig(1);
    let id = r.run(r.client.upload_begin("video/c")).expect("begin");
    let mut body = r.buffer();
    fill(&mut body, 3);
    let (part, body) = r.run(r.client.upload_part("video/c", &id, 1, body, LEN));
    let part = part.expect("part");
    assert_eq!(r.run(r.client.upload_abort("video/c", &id)), Ok(()));
    // Aborting what is already gone succeeds.
    assert_eq!(r.run(r.client.upload_abort("video/c", &id)), Ok(()));
    let (completed, _body) = r.run(r.client.upload_complete("video/c", &id, &[part], body));
    assert!(matches!(completed, Err(Error::Store(_))), "{completed:?}");
    assert_eq!(r.lanes[0].server.object("/video/c"), None);
}

#[test]
fn requests_that_cannot_be_sent_fail_at_once_and_return_the_buffer() {
    let mut r = rig(1);
    let id = r.run(r.client.upload_begin("video/d")).expect("begin");
    let body = r.buffer();
    let size = body.len();
    let (zero, body) = r.run(r.client.upload_part("video/d", &id, 0, body, LEN));
    assert_eq!(zero, Err(Error::BadPart));
    let (high, body) = r.run(r.client.upload_part("video/d", &id, 10_001, body, LEN));
    assert_eq!(high, Err(Error::BadPart));
    let (long, body) = r.run(r.client.upload_part("video/d", &id, 1, body, size + 1));
    assert_eq!(long, Err(Error::TooLong));

    let part = |number| Part::new(number, PartTag::new(b"\"tag\"").unwrap(), None);
    let (unordered, body) =
        r.run(
            r.client
                .upload_complete("video/d", &id, &[part(2), part(1)], body),
        );
    assert_eq!(unordered, Err(Error::BadRange));
    // More parts than the list of them has room for in one buffer.
    let many: Vec<Part> = (1..=2000).map(part).collect();
    let (too_many, body) = r.run(r.client.upload_complete("video/d", &id, &many, body));
    assert_eq!(too_many, Err(Error::TooLong));
    assert_eq!(body.len(), size);
    assert_eq!(r.run(r.client.upload_abort("video/d", &id)), Ok(()));
}
