// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! AWS-LC uses its 8-way AES-GCM on the cores it recognises and on the
//! listed ones, nowhere else, and records sealed with it open again.

use longshore_tls::record::{Direction, Suite, RECORD_MAX};
use longshore_tls::wide_gcm;

/// Whether this CPU has the instructions the 8-way routine needs.
fn capable() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
            && std::arch::is_aarch64_feature_detected!("pmull")
            && std::arch::is_aarch64_feature_detected!("sha3")
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

/// Main ID Register values: implementer, variant, architecture 0xF, part,
/// revision.
const fn midr(implementer: u64, part: u64) -> u64 {
    (implementer << 24) | (0xF << 16) | (part << 4) | 1
}

#[test]
fn only_apple_cores_and_the_cortex_x925_are_listed() {
    // An Apple M3 Max as a Linux guest reads it (OrbStack hides the part
    // number), and an Apple core with its part number.
    assert!(wide_gcm::listed(0x610F_0000));
    assert!(wide_gcm::listed(midr(0x61, 0x048)));
    // Cortex-X925 (DGX Spark).
    assert!(wide_gcm::listed(midr(0x41, 0xD85)));
    // Not measured: Cortex-A725 (the Spark's other cores), Neoverse N2.
    assert!(!wide_gcm::listed(midr(0x41, 0xD87)));
    assert!(!wide_gcm::listed(midr(0x41, 0xD49)));
    // Recognised by AWS-LC itself, or without SHA3: Neoverse V1, V2, N1.
    assert!(!wide_gcm::listed(midr(0x41, 0xD40)));
    assert!(!wide_gcm::listed(midr(0x41, 0xD4F)));
    assert!(!wide_gcm::listed(midr(0x41, 0xD0C)));
    // Same part number, another implementer.
    assert!(!wide_gcm::listed(midr(0x51, 0xD85)));
}

#[test]
fn the_8_way_routine_is_used_where_aws_lc_or_the_list_picks_it() {
    // What AWS-LC chose by itself; nothing else in this test binary has set
    // a key yet.
    let own = wide_gcm::active();
    let listed = wide_gcm::midr().is_some_and(wide_gcm::listed) && capable();
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        // AWS-LC recognises Apple M cores under macOS without help.
        assert!(own, "AWS-LC did not pick the 8-way routine under macOS");
    }
    let expected = own || listed;
    assert_eq!(wide_gcm::enable(), expected);
    assert_eq!(wide_gcm::active(), expected);
    // A second call changes nothing.
    assert_eq!(wide_gcm::enable(), expected);
    eprintln!(
        "midr {:x?}, AWS-LC's own choice {own}, listed {listed}, 8-way {}",
        wide_gcm::midr(),
        wide_gcm::active()
    );

    // Records sealed with whichever routine is in use open again; 256 bytes
    // and up go through the 8-way routine.
    let (key, iv) = ([7u8; 16], [9u8; 12]);
    let (mut seal, mut open) = (Direction::new(), Direction::new());
    seal.set_keys(Suite::Aes128Gcm, &key, &iv, 0).unwrap();
    open.set_keys(Suite::Aes128Gcm, &key, &iv, 0).unwrap();
    for len in [1usize, 255, 256, 4096, 16384] {
        let content: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
        let mut record = vec![0u8; RECORD_MAX];
        let n = seal.seal(23, &content, &mut record).unwrap();
        let mut back = vec![0u8; RECORD_MAX];
        let (kind, got) = open.open(&record[..n], &mut back).unwrap();
        assert_eq!((kind, &back[..got]), (23, &content[..]), "length {len}");
    }
}
