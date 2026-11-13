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

#[cfg(test)]
mod tests;
