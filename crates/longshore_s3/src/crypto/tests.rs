// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn hex_string(b: &[u8]) -> String {
    let mut out = vec![0u8; b.len() * 2];
    hex(b, &mut out);
    String::from_utf8(out).unwrap()
}

// FIPS 180-4 examples.
#[test]
fn sha256_known_answers() {
    assert_eq!(
        hex_string(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex_string(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex_string(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    let mut h = Sha256::new();
    for _ in 0..1000 {
        h.update(&[b'a'; 1000]);
    }
    assert_eq!(
        hex_string(&h.finish()),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        "one million 'a'"
    );
}

#[test]
fn incremental_equals_one_shot() {
    let data: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
    for split in [0, 1, 55, 56, 63, 64, 65, 500, 1000] {
        let mut h = Sha256::new();
        h.update(&data[..split]).update(&data[split..]);
        assert_eq!(h.finish(), sha256(&data), "split at {split}");
    }
}

// RFC 4231 test cases 1, 2, 3 and 6 (key longer than the block size).
#[test]
fn hmac_sha256_known_answers() {
    assert_eq!(
        hex_string(&hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hex_string(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        hex_string(&hmac_sha256(&[0xaa; 20], &[0xdd; 50])),
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
    );
    assert_eq!(
        hex_string(&hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}
