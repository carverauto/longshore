// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What the handshake wrapper answers to input that is not a handshake:
//! bytes that are not records, records that are not what TLS allows at that
//! point, alerts, and certificates or keys that cannot be used.

use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup, TlsError};

const NAME: &str = "objects.test";

fn client() -> Handshake {
    Handshake::client(&ClientSetup::with_public_roots(), NAME).unwrap()
}

fn identity() -> (Vec<u8>, Vec<u8>) {
    let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
    (k.cert.der().to_vec(), k.signing_key.serialize_der())
}

#[test]
fn a_record_header_is_judged_once_it_is_whole() {
    // Fewer than five bytes say nothing yet, whatever they are.
    for partial in [&b""[..], b"H", b"HTTP", &[22, 3, 3, 0]] {
        assert_eq!(client().read(partial), Ok(0), "{partial:?}");
    }
    // The four record types TLS has, and no other.
    for bad_type in [0u8, 19, 24, 0x80, 0xff] {
        assert_eq!(
            client().read(&[bad_type, 3, 3, 0, 1, 0]),
            Err(TlsError::BadRecord),
            "type {bad_type}"
        );
    }
    assert_eq!(
        client().read(&[22, 2, 3, 0, 1, 0]),
        Err(TlsError::BadRecord),
        "not a TLS version"
    );
}

#[test]
fn a_record_longer_than_the_protocol_allows_is_refused_from_its_header() {
    // 2^14 + 256 bytes is the most a record may declare.
    assert_eq!(
        client().read(&[22, 3, 3, 0x41, 0x00]),
        Ok(0),
        "the largest record, not all here yet"
    );
    assert_eq!(
        client().read(&[22, 3, 3, 0x41, 0x01]),
        Err(TlsError::BadRecord)
    );
    assert_eq!(
        client().read(&[23, 3, 3, 0xff, 0xff]),
        Err(TlsError::BadRecord)
    );
}

#[test]
fn an_incomplete_record_is_left_unconsumed() {
    // A handshake record that declares four bytes and has three.
    let mut c = client();
    assert_eq!(c.read(&[22, 3, 3, 0, 4, 2, 0, 0]), Ok(0));
    assert!(!c.is_done());
}

#[test]
fn a_well_framed_record_that_is_not_a_handshake_fails_the_handshake() {
    // A ServerHello with nothing in it.
    assert_eq!(
        client().read(&[22, 3, 3, 0, 4, 2, 0, 0, 0]),
        Err(TlsError::Handshake)
    );
    // A fatal handshake_failure alert, as a server with no suite in common sends.
    assert_eq!(
        client().read(&[21, 3, 3, 0, 2, 2, 40]),
        Err(TlsError::Handshake)
    );
    // Application data before any keys exist.
    assert_eq!(
        client().read(&[23, 3, 3, 0, 3, 1, 2, 3]),
        Err(TlsError::Handshake)
    );
    // And a server that is sent something other than a ClientHello.
    let (cert, key) = identity();
    let setup = ServerSetup::new(&[&cert], &key).unwrap();
    assert_eq!(
        Handshake::server(&setup)
            .unwrap()
            .read(&[22, 3, 3, 0, 4, 2, 0, 0, 0]),
        Err(TlsError::Handshake)
    );
}

#[test]
fn a_failed_handshake_cannot_be_finished() {
    use longshore_tls::record::Direction;
    let mut c = client();
    assert_eq!(c.read(&[21, 3, 3, 0, 2, 2, 40]), Err(TlsError::Handshake));
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    assert_eq!(c.finish(&mut tx, &mut rx), Err(TlsError::NotDone));
    // No keys were installed.
    assert_eq!(
        tx.seal(23, b"x", &mut [0u8; 64]),
        Err(longshore_tls::record::RecordError::BadKey)
    );
}

#[test]
fn unusable_certificates_keys_and_names_are_setup_errors() {
    let (cert, key) = identity();
    assert_eq!(
        ClientSetup::with_roots(&[b"not a certificate"]).err(),
        Some(TlsError::Setup)
    );
    assert_eq!(
        ServerSetup::new(&[&cert], b"not a key").err(),
        Some(TlsError::Setup)
    );
    assert!(ServerSetup::new(&[&cert], &key).is_ok());
    let setup = ClientSetup::with_roots(&[&cert]).unwrap();
    for bad in ["", "not a hostname", "a..b"] {
        assert_eq!(
            Handshake::client(&setup, bad).err(),
            Some(TlsError::Setup),
            "{bad:?}"
        );
    }
    // An address is a name a certificate can carry.
    assert!(Handshake::client(&setup, "192.0.2.1").is_ok());
}
