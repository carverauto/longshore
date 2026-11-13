// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! UTC timestamps without a calendar library.

/// Year, month, day and day of the week (0 = Sunday) for a day count since 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64, usize) {
    // Civil-from-days (Howard Hinnant's algorithm), valid for all dates we can see.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    // 1970-01-01 was a Thursday.
    (year, month, day, (days + 4).rem_euclid(7) as usize)
}

fn put(out: &mut [u8], at: usize, width: usize, mut v: u64) {
    for i in (0..width).rev() {
        out[at + i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// `YYYYMMDDTHHMMSSZ` for a Unix time in seconds, as SigV4 wants it.
pub fn amz_date(unix_secs: u64) -> [u8; 16] {
    let (year, month, day, _) = civil((unix_secs / 86_400) as i64);
    let secs = unix_secs % 86_400;
    let mut out = *b"00000000T000000Z";
    put(&mut out, 0, 4, year as u64);
    put(&mut out, 4, 2, month as u64);
    put(&mut out, 6, 2, day as u64);
    put(&mut out, 9, 2, secs / 3600);
    put(&mut out, 11, 2, secs / 60 % 60);
    put(&mut out, 13, 2, secs % 60);
    out
}

/// `Thu, 12 Nov 2026 00:00:00 GMT` for a Unix time in seconds: the HTTP date
/// form Azure wants in `x-ms-date`.
pub fn http_date(unix_secs: u64) -> [u8; 29] {
    let (year, month, day, weekday) = civil((unix_secs / 86_400) as i64);
    let secs = unix_secs % 86_400;
    let mut out = *b"Www, 00 Mmm 0000 00:00:00 GMT";
    out[..3].copy_from_slice(&b"SunMonTueWedThuFriSat"[weekday * 3..weekday * 3 + 3]);
    let m = (month - 1) as usize * 3;
    out[8..11].copy_from_slice(&b"JanFebMarAprMayJunJulAugSepOctNovDec"[m..m + 3]);
    put(&mut out, 5, 2, day as u64);
    put(&mut out, 12, 4, year as u64);
    put(&mut out, 17, 2, secs / 3600);
    put(&mut out, 20, 2, secs / 60 % 60);
    put(&mut out, 23, 2, secs % 60);
    out
}

/// The Unix time of an HTTP date in the form [`http_date`] writes, which is
/// the form `Last-Modified` uses. `None` for anything else.
pub fn parse_http_date(text: &[u8]) -> Option<u64> {
    if text.len() != 29 || &text[3..5] != b", " || &text[25..] != b" GMT" {
        return None;
    }
    let number = |at: usize, width: usize| -> Option<u64> {
        text[at..at + width].iter().try_fold(0u64, |n, d| {
            d.is_ascii_digit().then(|| n * 10 + (d - b'0') as u64)
        })
    };
    let separators = [(7, b' '), (11, b' '), (16, b' '), (19, b':'), (22, b':')];
    if separators.iter().any(|(at, b)| text[*at] != *b) {
        return None;
    }
    let month = b"JanFebMarAprMayJunJulAugSepOctNovDec"
        .chunks(3)
        .position(|m| m == &text[8..11])? as i64
        + 1;
    let (day, year) = (number(5, 2)? as i64, number(12, 4)? as i64);
    let (hour, minute, second) = (number(17, 2)?, number(20, 2)?, number(23, 2)?);
    if year < 1970 || day == 0 || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days-from-civil, the inverse of `civil`.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    // A day that does not exist in that month does not survive the round trip.
    let back = civil(days);
    if (back.0, back.1, back.2) != (year, month, day) {
        return None;
    }
    Some(days as u64 * 86_400 + hour * 3600 + minute * 60 + second.min(59))
}

#[cfg(test)]
mod tests;
