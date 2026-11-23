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
    let unsectioned =
        Credentials::parse(b"aws_secret_access_key = s\r\naws_access_key_id = a\r\n").unwrap();
    assert_eq!(
        (unsectioned.access_key(), unsectioned.secret_key()),
        ("a", "s")
    );
    // A later value replaces an earlier one.
    let repeated = Credentials::parse(
        b"aws_access_key_id=old\naws_access_key_id=new\naws_secret_access_key=s",
    )
    .unwrap();
    assert_eq!(repeated.access_key(), "new");
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
        Credentials::bearer(&"x".repeat(TOKEN_MAX)).map(|held| held.secret_key().len()),
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
    let after_prod = Credentials::parse(b"[prod]\naws_access_key_id = P\naws_secret_access_key = p\n[default] # mine\naws_access_key_id = D\naws_secret_access_key = d\n").unwrap();
    assert_eq!(
        (after_prod.access_key(), after_prod.secret_key()),
        ("D", "d")
    );
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

/// A value is everything after the first `=`. Session tokens are base64 and
/// commonly end in one or two more.
#[test]
fn a_value_keeps_the_equals_signs_inside_it() {
    let c = Credentials::parse(
        b"aws_access_key_id = A\naws_secret_access_key=a=b\naws_session_token = abc/def+ghi==\n",
    )
    .unwrap();
    assert_eq!(
        (c.access_key(), c.secret_key(), c.session_token()),
        ("A", "a=b", "abc/def+ghi==")
    );
    // A name must be the whole name.
    assert!(Credentials::parse(b"aws_access_key_id2 = A\naws_secret_access_key = s\n").is_none());
    assert!(
        Credentials::parse(b"AWS_ACCESS_KEY_ID = A\naws_secret_access_key = s\n").is_none(),
        "names are lowercase"
    );
}

/// What the code does today about where keys are read from, written down.
#[test]
fn which_lines_are_in_scope() {
    let pair = |c: Credentials| (c.access_key().to_string(), c.secret_key().to_string());
    // Keys before the first section count as well as those under
    // `[default]`, and the later value wins.
    let c = Credentials::parse(
        b"aws_access_key_id = TOP\naws_secret_access_key = top\n[default]\naws_access_key_id = DEF\n",
    )
    .unwrap();
    assert_eq!(pair(c), ("DEF".to_string(), "top".to_string()));
    // A `[default]` section given twice is read both times.
    let twice = Credentials::parse(
        b"[default]\naws_access_key_id = ONE\n[other]\naws_access_key_id = NO\n[default]\naws_secret_access_key = two\n",
    )
    .unwrap();
    assert_eq!(pair(twice), ("ONE".to_string(), "two".to_string()));
    // Spaces inside the brackets do not make it another section.
    let spaced =
        Credentials::parse(b"[other]\n[ default ]\naws_access_key_id=A\naws_secret_access_key=s\n")
            .unwrap();
    assert_eq!(pair(spaced), ("A".to_string(), "s".to_string()));
    for other in ["[Default]", "[profile default]", "[default2]"] {
        let text = format!("{other}\naws_access_key_id=A\naws_secret_access_key=s\n");
        assert!(Credentials::parse(text.as_bytes()).is_none(), "{other}");
    }
    // A comment after a value is part of the value, which a space makes unusable.
    assert!(
        Credentials::parse(b"aws_access_key_id = AKID # note\naws_secret_access_key = s\n")
            .is_none()
    );
}

#[test]
fn each_part_is_held_up_to_its_limit() {
    let at = "a".repeat(FIELD_MAX);
    let c = Credentials::new(&at, &at).unwrap();
    assert_eq!((c.access_key(), c.secret_key()), (at.as_str(), at.as_str()));
    let over = "a".repeat(FIELD_MAX + 1);
    assert!(Credentials::new(&over, "s").is_none());
    assert!(Credentials::new("a", &over).is_none());
    assert!(Credentials::new("", "s").is_none());
    assert!(Credentials::new("a", "").is_none());
    let token = "t".repeat(SESSION_MAX);
    assert_eq!(
        Credentials::with_session("a", "s", &token)
            .unwrap()
            .session_token(),
        token
    );
    for bad in ["caf\u{e9}", "a\u{7f}", "a\0"] {
        assert!(Credentials::new(bad, "s").is_none(), "{bad:?}");
        assert!(Credentials::new("a", bad).is_none(), "{bad:?}");
    }
}
