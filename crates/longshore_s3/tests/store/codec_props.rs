// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) for timestamps, the credentials file and the
//! account-key decoder.

use hegel::generators as gs;
use hegel::TestCase;
use longshore_s3::creds::{Credentials, FIELD_MAX};
use longshore_s3::sharedkey::{decode_key, KEY_MAX};
use longshore_s3::time::{amz_date, http_date, parse_http_date};

/// The last second of the year 9999, the last a four-digit year can show.
const TIME_MAX: u64 = 253_402_300_799;

fn leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn month_days(year: u64, month: u64) -> u64 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Year, month, day and weekday (0 = Sunday) of a Unix time, by counting
/// years and months forward from 1970-01-01, a Thursday.
fn calendar(unix_secs: u64) -> (u64, u64, u64, usize) {
    let total = unix_secs / 86_400;
    let (mut days, mut year, mut month) = (total, 1970, 1);
    while days >= 365 + u64::from(leap(year)) {
        days -= 365 + u64::from(leap(year));
        year += 1;
    }
    while days >= month_days(year, month) {
        days -= month_days(year, month);
        month += 1;
    }
    (year, month, days + 1, ((total + 4) % 7) as usize)
}

/// Both timestamp forms show the date and time a calendar counted out by
/// hand gives, and the HTTP form parses back to the instant.
#[hegel::test]
fn timestamps_agree_with_a_calendar_and_parse_back(tc: TestCase) {
    let t = tc.draw(gs::integers::<u64>().min_value(0).max_value(TIME_MAX));
    let (year, month, day, weekday) = calendar(t);
    let (h, m, s) = (t % 86_400 / 3600, t % 3600 / 60, t % 60);
    assert_eq!(
        String::from_utf8(amz_date(t).to_vec()).unwrap(),
        format!("{year:04}{month:02}{day:02}T{h:02}{m:02}{s:02}Z")
    );
    let weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let http = http_date(t);
    assert_eq!(
        String::from_utf8(http.to_vec()).unwrap(),
        format!(
            "{}, {day:02} {} {year:04} {h:02}:{m:02}:{s:02} GMT",
            weekdays[weekday],
            months[month as usize - 1]
        )
    );
    assert_eq!(parse_http_date(&http), Some(t));
}

/// A date is parsed only if it is one: any day number is accepted exactly
/// when that month of that year has such a day, and one changed byte
/// either still names an instant or is refused, never something else.
#[hegel::test]
fn only_real_dates_parse(tc: TestCase) {
    let t = tc.draw(gs::integers::<u64>().min_value(0).max_value(TIME_MAX));
    let (year, month, _, _) = calendar(t);
    let mut text = http_date(t);
    let day = tc.draw(gs::integers::<u64>().min_value(0).max_value(99));
    text[5] = b'0' + (day / 10) as u8;
    text[6] = b'0' + (day % 10) as u8;
    let real = (1..=month_days(year, month)).contains(&day);
    let parsed = parse_http_date(&text);
    assert_eq!(parsed.is_some(), real, "{}", String::from_utf8_lossy(&text));
    if let Some(back) = parsed {
        let (y, mo, d, _) = calendar(back);
        assert_eq!((y, mo, d, back % 86_400), (year, month, day, t % 86_400));
    }

    let mut damaged = http_date(t);
    let at = tc.draw(gs::integers::<usize>().min_value(0).max_value(28));
    damaged[at] = tc.draw(gs::integers::<u8>());
    if let Some(back) = parse_http_date(&damaged) {
        // The weekday is not checked, so only it may differ from what is
        // written for the instant read.
        assert_eq!(http_date(back)[3..], damaged[3..]);
    }
}

/// A word of 1 to `max` printable characters with no space among them.
fn graphic(tc: &TestCase, max: usize) -> String {
    tc.draw(gs::binary().min_size(1).max_size(max))
        .iter()
        .map(|b| (b'!' + b % 94) as char)
        .collect()
}

/// A file written the way the tools write it, with any spacing, comments
/// and other profiles around the default one, gives back its three values:
/// whatever printable characters they hold, `=`, `#` and `[` among them.
#[hegel::test]
fn a_credentials_file_gives_back_what_was_written(tc: TestCase) {
    let access = graphic(&tc, FIELD_MAX);
    let secret = graphic(&tc, FIELD_MAX);
    let session = tc.draw(gs::booleans()).then(|| graphic(&tc, 300));
    let pad = || -> &'static str {
        ["", " ", "\t", "  "][tc.draw(gs::integers::<usize>().min_value(0).max_value(3))]
    };
    let eol = if tc.draw(gs::booleans()) {
        "\n"
    } else {
        "\r\n"
    };
    let mut lines = vec![
        format!(
            "{}aws_access_key_id{}={}{access}{}",
            pad(),
            pad(),
            pad(),
            pad()
        ),
        format!("aws_secret_access_key{}={}{secret}", pad(), pad()),
    ];
    if let Some(token) = &session {
        lines.push(format!("aws_session_token{}={}{token}", pad(), pad()));
    }
    if tc.draw(gs::booleans()) {
        lines.reverse();
    }
    let mut file = String::new();
    if tc.draw(gs::booleans()) {
        file += &format!("# a comment{eol}[other]{eol}aws_access_key_id = NOT{eol}aws_secret_access_key = this{eol}aws_session_token = one{eol}");
    }
    let sectioned = tc.draw(gs::booleans());
    if sectioned || !file.is_empty() {
        file += &format!("[default]{}{eol}", pad());
    }
    for line in &lines {
        file += &format!("{line}{eol}");
        if tc.draw(gs::booleans()) {
            file += &format!("; between{eol}{eol}region = us-east-1{eol}");
        }
    }
    if tc.draw(gs::booleans()) {
        file += &format!("[later] # another{eol}aws_access_key_id = NOR{eol}aws_secret_access_key = this{eol}aws_session_token = two{eol}");
    }

    let parsed = Credentials::parse(file.as_bytes()).unwrap_or_else(|| panic!("{file:?}"));
    assert_eq!(
        (
            parsed.access_key(),
            parsed.secret_key(),
            parsed.session_token()
        ),
        (
            access.as_str(),
            secret.as_str(),
            session.as_deref().unwrap_or("")
        ),
        "{file:?}"
    );
    let direct = match &session {
        Some(token) => Credentials::with_session(&access, &secret, token),
        None => Credentials::new(&access, &secret),
    };
    assert_eq!(Some(parsed), direct);
    // The secret is never printed, unless the access key happens to spell it.
    let shown = format!("{parsed:?}");
    assert!(
        secret.len() < 8 || access.contains(&secret) || !shown.contains(&secret),
        "{shown}"
    );

    // Cut short before the last value is complete, it is refused or reads
    // as other credentials: never as these.
    let cut = tc.draw(
        gs::integers::<usize>()
            .min_value(0)
            .max_value(file.find(lines[0].trim()).unwrap()),
    );
    assert_ne!(Credentials::parse(&file.as_bytes()[..cut]), direct);
}

/// Any key of up to 64 bytes survives being written in base64 and decoded,
/// and the decoder accepts no second spelling of it.
#[hegel::test]
fn an_account_key_decodes_to_what_was_encoded(tc: TestCase) {
    let key = tc.draw(gs::binary().min_size(1).max_size(KEY_MAX));
    let mut text = [0u8; 88];
    let n = longshore_crc::base64(&key, &mut text);
    let text = &mut text[..n];
    let mut out = [0u8; KEY_MAX];
    assert_eq!(
        decode_key(core::str::from_utf8(text).unwrap(), &mut out),
        Some(key.len())
    );
    assert_eq!(&out[..key.len()], &key[..]);

    // One character changed: refused, or the key it then spells, which
    // encodes back to exactly the changed text.
    let at = tc.draw(gs::integers::<usize>().min_value(0).max_value(n - 1));
    text[at] = tc.draw(gs::integers::<u8>().min_value(0).max_value(127));
    let changed = core::str::from_utf8(text).unwrap();
    if let Some(len) = decode_key(changed, &mut out) {
        let mut again = [0u8; 88];
        let m = longshore_crc::base64(&out[..len], &mut again);
        assert_eq!(&again[..m], changed.as_bytes());
    }
}
