// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn parses_the_shared_credentials_format() {
    let c = Credentials::parse(b"# rotated 2026-11-12\n[default]\naws_access_key_id = AKIDEXAMPLE\naws_secret_access_key=wJalr/K7MDENG+bPxRfiCY\n\n[other]\naws_access_key_id = NOPE\naws_secret_access_key = NOPE\n").unwrap();
    assert_eq!(
        (c.access_key(), c.secret_key()),
        ("AKIDEXAMPLE", "wJalr/K7MDENG+bPxRfiCY")
    );
    // No sections at all.
    let c = Credentials::parse(b"aws_secret_access_key = s\r\naws_access_key_id = a\r\n").unwrap();
    assert_eq!((c.access_key(), c.secret_key()), ("a", "s"));
    // A later value replaces an earlier one.
    let c = Credentials::parse(
        b"aws_access_key_id=old\naws_access_key_id=new\naws_secret_access_key=s",
    )
    .unwrap();
    assert_eq!(c.access_key(), "new");
}

#[test]
fn rejects_incomplete_or_unusable_files() {
    for bad in [
        &b""[..],
        b"[default]\naws_access_key_id = a\n",
        b"[default]\naws_secret_access_key = s\n",
        b"[other]\naws_access_key_id=a\naws_secret_access_key=s\n",
        b"aws_access_key_id=\naws_secret_access_key=s",
        b"aws_access_key_id=has space\naws_secret_access_key=s",
        b"aws_access_key_id=a\naws_secret_access_key=\xff\xfe",
    ] {
        assert!(
            Credentials::parse(bad).is_none(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    let long = format!(
        "aws_access_key_id={}\naws_secret_access_key=s",
        "a".repeat(FIELD_MAX + 1)
    );
    assert!(Credentials::parse(long.as_bytes()).is_none());
    // A file caught half-written (truncated mid-line) is simply not accepted.
    assert!(Credentials::parse(b"[default]\naws_access_key_id = AKID\naws_secret_acc").is_none());
}

#[test]
fn debug_output_hides_the_secret() {
    let c = Credentials::new("AKID", "super-secret-value").unwrap();
    let shown = format!("{c:?}");
    assert!(
        shown.contains("AKID") && !shown.contains("super-secret-value"),
        "{shown}"
    );
}

#[test]
fn bearer_tokens_are_held_whole_or_refused() {
    let token = "ya29.".to_string() + &"A1b2-_".repeat(170);
    let c = Credentials::parse_bearer(format!("  {token}\n").as_bytes()).unwrap();
    assert_eq!((c.access_key(), c.secret_key()), ("", token.as_str()));
    assert_eq!(
        Credentials::bearer(&"x".repeat(TOKEN_MAX)).map(|c| c.secret_key().len()),
        Some(TOKEN_MAX)
    );
    for bad in [
        String::new(),
        "x".repeat(TOKEN_MAX + 1),
        "two words".into(),
        "line\nbreak".into(),
        "tab\there".into(),
    ] {
        assert!(
            Credentials::bearer(&bad).is_none(),
            "{:?}",
            &bad[..bad.len().min(12)]
        );
    }
    assert!(Credentials::parse_bearer(b"   \n").is_none());
    assert!(
        !format!("{c:?}").contains("ya29"),
        "Debug must not print the token"
    );
}

/// A section header may carry a comment. Missing that would leave the
/// parser in the previous section, and another profile's keys would
/// replace the default ones.
#[test]
fn a_section_header_with_a_comment_still_ends_the_default_section() {
    let text = b"[default]\naws_access_key_id = AKIDDEFAULT\naws_secret_access_key = secretdefault\n[prod] ; production\naws_access_key_id = AKIDPROD\naws_secret_access_key = secretprod\n[other] # note\naws_access_key_id = AKIDOTHER\n";
    let c = Credentials::parse(text).unwrap();
    assert_eq!(
        (c.access_key(), c.secret_key()),
        ("AKIDDEFAULT", "secretdefault")
    );
    let c = Credentials::parse(b"[prod]\naws_access_key_id = P\naws_secret_access_key = p\n[default] # mine\naws_access_key_id = D\naws_secret_access_key = d\n").unwrap();
    assert_eq!((c.access_key(), c.secret_key()), ("D", "d"));
}

#[test]
fn temporary_keys_carry_their_session_token() {
    let token = "not-a-real-token-".repeat(60);
    let text = format!("[default]\naws_access_key_id = TEMPKEYID\naws_secret_access_key = tempsecret\naws_session_token = {token}\n");
    let c = Credentials::parse(text.as_bytes()).unwrap();
    assert_eq!(
        (c.access_key(), c.secret_key(), c.session_token()),
        ("TEMPKEYID", "tempsecret", token.as_str())
    );
    // Long-lived keys have none.
    assert_eq!(Credentials::new("AKIA", "s").unwrap().session_token(), "");
    // A token is part of the identity: the same keys with another token differ.
    assert_ne!(
        c,
        Credentials::with_session("TEMPKEYID", "tempsecret", "other").unwrap()
    );
    for bad in [
        String::new(),
        "x".repeat(SESSION_MAX + 1),
        "two words".to_string(),
    ] {
        assert!(Credentials::with_session("A", "s", &bad).is_none());
    }
    // A file whose token is unusable is refused whole, not read as long-lived keys.
    assert!(Credentials::parse(
        b"aws_access_key_id = A\naws_secret_access_key = s\naws_session_token = two words\n"
    )
    .is_none());
    assert!(
        !format!("{c:?}").contains("not-a-real-token"),
        "Debug must not print the token"
    );
}
