// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Credential rotation in the simulator: the store rereads a credentials
//! file, validates new credentials against the server, and only then uses
//! them. Allocation is forbidden while operations run.

mod creds_world;
use creds_world::*;
use longshore::io::alloc_guard::Guard;
use longshore::sim::Faults;
use longshore::store::s3::Outcome;

#[global_allocator]
static ALLOC: Guard = Guard;

#[test]
fn new_credentials_are_adopted_only_after_the_store_accepts_them() {
    let mut w = world(1, Faults::default());
    // No file yet: a failed read, and the configured credentials stay in use.
    w.settle(REREAD_MS + 50);
    assert_eq!(
        (
            w.s3.access_key(),
            w.s3.credential_rotations,
            w.s3.credential_failures
        ),
        (KEY_A, 0, 1)
    );
    assert_eq!(w.store().outcome, Outcome::Stored);

    // The file now names a key the server does not accept yet.
    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(
        (w.s3.access_key(), w.s3.credential_rotations),
        (KEY_A, 0),
        "refused credentials must not be adopted"
    );
    assert!(w.s3.credential_failures >= 2);
    assert_eq!(
        w.store().outcome,
        Outcome::Stored,
        "requests keep working on the old credentials"
    );

    // The operator enables the new key alongside the old one.
    w.server.allow_access_keys(&[KEY_A, KEY_B]);
    w.settle(REREAD_MS + 50);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));

    // The old key is revoked; requests are signed with the new one.
    w.server.allow_access_keys(&[KEY_B]);
    assert_eq!(w.store().outcome, Outcome::Stored);
    assert_eq!(w.store().attempts, 1);

    // An unchanged file causes no further validation requests.
    let (requests, failures) = (w.server.requests, w.s3.credential_failures);
    w.settle(5 * REREAD_MS);
    assert_eq!(
        (
            w.server.requests,
            w.s3.credential_rotations,
            w.s3.credential_failures
        ),
        (requests, 1, failures),
        "an unchanged file must not trigger validation"
    );

    // A half-written or garbage file is counted and ignored.
    w.io.set_file(
        w.creds,
        Some(b"[default]\naws_access_key_id = AKIDHALF\naws_secret_acc"),
    );
    w.settle(REREAD_MS + 50);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));
    assert!(w.s3.credential_failures > failures);
    assert_eq!(w.store().outcome, Outcome::Stored);
}

/// A file longer than one read takes cannot be known to be complete, even
/// when the part read holds a whole key pair, so it is refused. The read is
/// bounded by the client's buffer here, which is smaller than the file limit.
#[test]
fn a_file_longer_than_one_read_is_refused() {
    let buf = 4096;
    let mut w = world_with_buffers(1, Faults::default(), buf);
    w.server.allow_access_keys(&[KEY_A, KEY_B]);
    let mut text = file(KEY_B);
    while text.len() <= buf as usize {
        text.push_str("# padding that pushes the file past one read\n");
    }
    w.io.set_file(w.creds, Some(text.as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(
        (w.s3.access_key(), w.s3.credential_rotations),
        (KEY_A, 0),
        "a file longer than one read must not be adopted"
    );
    assert!(w.s3.credential_failures >= 1);

    // The same key pair in a file that fits is adopted.
    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!((w.s3.access_key(), w.s3.credential_rotations), (KEY_B, 1));
}

/// The whole rotation, with requests running throughout and network faults
/// injected: add the new key, rotate the file, revoke the old key. No
/// request may fail.
#[test]
fn rotation_under_load_fails_no_request() {
    let faults = Faults {
        short_send: 200_000,
        partial_recv: 200_000,
        delay: 200_000,
        ..Faults::default()
    };
    for seed in 0..20 {
        let mut w = world(seed, faults);
        let mut stage = 0;
        for i in 0..120 {
            let d = w.store();
            assert_eq!(
                d.outcome,
                Outcome::Stored,
                "seed {seed}, request {i}, stage {stage}: {d:?}"
            );
            match (stage, i) {
                (0, 20) => {
                    w.server.allow_access_keys(&[KEY_A, KEY_B]);
                    w.io.set_file(w.creds, Some(file(KEY_B).as_bytes()));
                    stage = 1;
                }
                // Revoke the old key only once the client has moved off it.
                (1, _) if w.s3.credential_rotations == 1 => {
                    w.server.allow_access_keys(&[KEY_B]);
                    stage = 2;
                }
                _ => {}
            }
            w.settle(10);
        }
        assert_eq!(
            (stage, w.s3.access_key(), w.s3.credential_rotations),
            (2, KEY_B, 1),
            "seed {seed}: rotation did not complete"
        );
    }
}

/// Temporary keys work only with their session token: the store sends it
/// with every request, and picks up a new token when the file changes.
#[test]
fn temporary_keys_send_their_session_token_and_rotate_with_it() {
    let mut w = world(7, Faults::default());
    // The server now insists on a token. The configured keys have none.
    w.server.require_session_token("token-one");
    assert_eq!(
        w.store().outcome,
        Outcome::Fatal(longshore::store::s3::Fatal::Status(403))
    );

    // The file supplies the same keys with a token: a change, so it is validated and adopted.
    let with_token = |token: &str| {
        format!("[default]\naws_access_key_id = {KEY_A}\naws_secret_access_key = secret-for-{KEY_A}\naws_session_token = {token}\n")
    };
    w.io.set_file(w.creds, Some(with_token("token-one").as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(w.s3.credential_rotations, 1);
    assert_eq!(w.store().outcome, Outcome::Stored);

    // The token is replaced, as it is every few hours in practice.
    w.server.require_session_token("token-two");
    w.io.set_file(w.creds, Some(with_token("token-two").as_bytes()));
    w.settle(REREAD_MS + 50);
    assert_eq!(w.s3.credential_rotations, 2);
    assert_eq!(w.store().outcome, Outcome::Stored);
}
