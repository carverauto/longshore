// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::{amz_date, http_date};

fn s(t: u64) -> String {
    String::from_utf8(amz_date(t).to_vec()).unwrap()
}

#[test]
fn formats_known_instants() {
    assert_eq!(s(0), "19700101T000000Z");
    assert_eq!(
        s(1_440_938_160),
        "20150830T123600Z",
        "the SigV4 test-suite timestamp"
    );
    assert_eq!(
        s(951_782_400),
        "20000229T000000Z",
        "leap day in a century leap year"
    );
    assert_eq!(s(951_868_799), "20000229T235959Z");
    assert_eq!(
        s(4_107_542_399),
        "21000228T235959Z",
        "2100 is not a leap year"
    );
    assert_eq!(s(4_107_542_400), "21000301T000000Z");
    assert_eq!(s(1_794_441_600), "20261112T000000Z");
}

/// Expected values from Python's `email.utils.formatdate`.
#[test]
fn formats_http_dates() {
    let d = |t: u64| String::from_utf8(http_date(t).to_vec()).unwrap();
    assert_eq!(d(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(d(1_435_361_952), "Fri, 26 Jun 2015 23:39:12 GMT");
    assert_eq!(d(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
    assert_eq!(d(4_107_542_399), "Sun, 28 Feb 2100 23:59:59 GMT");
    assert_eq!(d(1_794_441_600), "Thu, 12 Nov 2026 00:00:00 GMT");
}
