// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn published_check_values() {
    // The standard check string from the CRC catalogue.
    assert_eq!(crc64_nvme(b"123456789"), 0xae8b_1486_0a79_9888);
    assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    assert_eq!(crc64_nvme(b""), 0);
    assert_eq!(crc32c(b""), 0);
    // RFC 3720 B.4: 32 bytes of zeros and of ones.
    assert_eq!(crc32c(&[0u8; 32]), 0x8a91_36aa);
    assert_eq!(crc32c(&[0xffu8; 32]), 0x62a8_ab43);
}

#[test]
fn base64_rfc4648_vectors() {
    for (input, expected) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        let mut out = [0u8; 16];
        let n = base64(input.as_bytes(), &mut out);
        assert_eq!(&out[..n], expected.as_bytes(), "{input:?}");
    }
}

#[test]
fn header_values() {
    // Big-endian bytes of the check value, base64 encoded.
    assert_eq!(&crc64_header_value(0xae8b_1486_0a79_9888), b"rosUhgp5mIg=");
    // Azure: the same eight bytes in the opposite order.
    assert_eq!(
        &crc64_azure_header_value(0xae8b_1486_0a79_9888),
        b"iJh5CoYUi64="
    );
    assert_eq!(&crc32c_header_value(0xe306_9283), b"4waSgw==");
    assert_eq!(&crc64_header_value(0), b"AAAAAAAAAAA=");
}

#[test]
fn base64_reaches_the_last_two_letters_of_the_alphabet() {
    // Indices 62 and 63: a URL-safe alphabet would differ only here.
    for (input, expected) in [
        (&[0xfbu8, 0xff, 0xbf][..], "+/+/"),
        (&[0xff][..], "/w=="),
        (&[0xfb, 0xf0][..], "+/A="),
    ] {
        let mut out = [0u8; 16];
        let n = base64(input, &mut out);
        assert_eq!(&out[..n], expected.as_bytes(), "{input:?}");
    }
    // Every letter, in order, from the 48 bytes that spell the alphabet.
    let mut all = [0u8; 48];
    for (i, chunk) in all.chunks_mut(3).enumerate() {
        let i = i as u32 * 4;
        let word = i << 18 | (i + 1) << 12 | (i + 2) << 6 | (i + 3);
        chunk.copy_from_slice(&word.to_be_bytes()[1..]);
    }
    let mut out = [0u8; 64];
    assert_eq!(base64(&all, &mut out), 64);
    assert_eq!(
        &out[..],
        &b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[..]
    );
}

#[test]
fn header_values_with_high_bits_in_every_byte() {
    assert_eq!(&crc32c_header_value(0xfbff_bfff), b"+/+//w==");
    assert_eq!(&crc64_header_value(u64::MAX), b"//////////8=");
    assert_eq!(&crc64_azure_header_value(u64::MAX), b"//////////8=");
    assert_eq!(&crc64_header_value(1), b"AAAAAAAAAAE=");
    assert_eq!(&crc64_azure_header_value(1), b"AQAAAAAAAAA=");
}
