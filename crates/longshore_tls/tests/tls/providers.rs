// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The providers compiled into this build agree: they seal the same bytes,
//! open each other's records, refuse tampered ones, complete handshakes with
//! each other, and stay inside the allocation rules.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_tls::handshake::{ClientSetup, Handshake, ServerSetup, TlsError};
use longshore_tls::record::{
    Direction, Provider, RecordError, Suite, CONTENT_APPLICATION_DATA, CONTENT_HANDSHAKE,
    HEADER_LEN, RECORD_MAX,
};

#[global_allocator]
static ALLOC: Guard = Guard;

const NAME: &str = "objects.test";
const LENGTHS: [usize; 5] = [1, 255, 256, 4096, 16_384];
const IV: [u8; 12] = [9; 12];

#[cfg(test)]
fn compiled() -> impl Iterator<Item = Provider> {
    Provider::ALL.into_iter().filter(|p| p.compiled())
}

#[cfg(test)]
fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

#[cfg(test)]
/// Every way this build can key a direction from `key`: AWS-LC and ring
/// take raw bytes; rustls takes its own key type, which only a 32-byte
/// (AES-256) key can be built into outside a handshake.
fn keyed(suite: Suite, key: &[u8], sending: bool) -> Vec<(Provider, Direction)> {
    let mut out = Vec::new();
    for p in [Provider::AwsLc, Provider::Ring]
        .into_iter()
        .filter(|p| p.compiled())
    {
        let mut d = Direction::new();
        d.set_keys_with(p, suite, key, &IV, 0).unwrap();
        out.push((p, d));
    }
    #[cfg(feature = "rustls")]
    if let Ok(key32) = <[u8; 32]>::try_from(key) {
        use longshore_tls::record::Role;
        let mut d = Direction::new();
        let role = if sending { Role::Seal } else { Role::Open };
        d.set_rustls_keys(suite, key32.into(), &IV, 0, role)
            .unwrap();
        out.push((Provider::Rustls, d));
    }
    let _: bool = sending;
    out
}

#[test]
fn providers_seal_identical_records_and_open_each_others() {
    for (suite, key) in [
        (Suite::Aes128Gcm, &[0x5a; 32][..16]),
        (Suite::Aes256Gcm, &[0x5a; 32][..]),
    ] {
        for len in LENGTHS {
            let body = content(len);
            let mut sealed_by = Vec::new();
            for (p, mut tx) in keyed(suite, key, true) {
                let mut rec = vec![0u8; RECORD_MAX];
                let n = tx.seal(CONTENT_APPLICATION_DATA, &body, &mut rec).unwrap();
                rec.truncate(n);
                sealed_by.push((p, rec));
            }
            let Some((_, first)) = sealed_by.first() else {
                continue; // rustls alone, AES-128: no raw-key provider in this build
            };
            for (p, rec) in &sealed_by {
                assert_eq!(rec, first, "{p:?} {suite:?} {len}: records differ");
            }
            for (sealer, rec) in &sealed_by {
                for (opener, mut rx) in keyed(suite, key, false) {
                    let mut dst = vec![0u8; rx.open_room(rec.len())];
                    let got = rx.open(rec, &mut dst);
                    assert_eq!(
                        got,
                        Ok((CONTENT_APPLICATION_DATA, len)),
                        "{sealer:?} -> {opener:?}"
                    );
                    assert_eq!(&dst[..len], &body[..], "{sealer:?} -> {opener:?} {len}");
                }
                for (opener, mut rx) in keyed(suite, key, false) {
                    let mut copy = rec.clone();
                    let got = rx.open_in_place(&mut copy);
                    assert_eq!(
                        got,
                        Ok((CONTENT_APPLICATION_DATA, len)),
                        "{sealer:?} -> {opener:?}"
                    );
                    assert_eq!(&copy[HEADER_LEN..HEADER_LEN + len], &body[..]);
                }
            }
        }
    }
}

#[test]
fn every_provider_refuses_a_tampered_record() {
    let key = [0x33u8; 32];
    let (_, mut tx) = keyed(Suite::Aes256Gcm, &key, true).remove(0);
    let mut rec = vec![0u8; RECORD_MAX];
    let n = tx
        .seal(CONTENT_APPLICATION_DATA, &content(4096), &mut rec)
        .unwrap();
    rec.truncate(n);
    for at in [HEADER_LEN, HEADER_LEN + 2000, n - 1] {
        let mut bad = rec.clone();
        bad[at] ^= 1;
        for (opener, mut rx) in keyed(Suite::Aes256Gcm, &key, false) {
            let mut dst = vec![0u8; rx.open_room(n)];
            assert_eq!(
                rx.open(&bad, &mut dst),
                Err(RecordError::BadRecord),
                "{opener:?} at {at}"
            );
        }
        for (opener, mut rx) in keyed(Suite::Aes256Gcm, &key, false) {
            let mut copy = bad.clone();
            assert_eq!(
                rx.open_in_place(&mut copy),
                Err(RecordError::BadRecord),
                "{opener:?}"
            );
        }
    }
}

#[cfg(test)]
struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

#[cfg(test)]
fn identity() -> Identity {
    let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
    Identity {
        cert: k.cert.der().to_vec(),
        key: k.signing_key.serialize_der(),
    }
}

#[cfg(test)]
/// Run a handshake between fixed buffers, so that the loop itself does not
/// allocate. Returns what the server sent after finishing (its session
/// tickets), still to be opened by the client's record layer.
fn handshake(c: &mut Handshake, s: &mut Handshake, a: &mut [u8], b: &mut [u8]) -> usize {
    let (mut an, mut bn) = (0usize, 0usize);
    for _ in 0..1000 {
        an += c.write(&mut a[an..]);
        bn += s.write(&mut b[bn..]);
        let s_used = s.read(&a[..an]).unwrap();
        a.copy_within(s_used..an, 0);
        an -= s_used;
        let c_used = c.read(&b[..bn]).unwrap();
        b.copy_within(c_used..bn, 0);
        bn -= c_used;
        if c.is_done() && s.is_done() && an == 0 {
            return bn;
        }
    }
    panic!("handshake did not finish");
}

#[cfg(test)]
/// A client with `cp` against a server with `sp`: handshake, the server's
/// tickets opened, and records both ways. Every step allocation-free except
/// those inside the sanctioned exception.
fn connect(id: &Identity, cp: Provider, sp: Provider) -> Result<(), TlsError> {
    let cs = ClientSetup::with_roots_using(cp, &[&id.cert])?;
    let ss = ServerSetup::new_using(sp, &[&id.cert], &id.key)?;
    let (mut ctx, mut crx, mut stx, mut srx) = (
        Direction::new(),
        Direction::new(),
        Direction::new(),
        Direction::new(),
    );
    let (mut a, mut b) = (vec![0u8; 32_768], vec![0u8; 32_768]);
    let mut out = vec![0u8; RECORD_MAX];
    let bodies: Vec<Vec<u8>> = LENGTHS.iter().map(|n| content(*n)).collect();
    let ok = sealed(|| -> Result<bool, TlsError> {
        let (mut c, mut s) = (Handshake::client(&cs, NAME)?, Handshake::server(&ss)?);
        let mut tickets = handshake(&mut c, &mut s, &mut a, &mut b);
        c.finish(&mut ctx, &mut crx)?;
        s.finish(&mut stx, &mut srx)?;
        let mut ok = ctx.provider() == Some(cp) && srx.provider() == Some(sp);
        while tickets > 0 {
            let len = longshore_tls::record::record_len(&b[..tickets]).map_err(TlsError::Record)?;
            let len = len.unwrap_or(tickets);
            let (kind, _) = crx.open_in_place(&mut b[..len]).map_err(TlsError::Record)?;
            ok &= kind == CONTENT_HANDSHAKE;
            b.copy_within(len..tickets, 0);
            tickets -= len;
        }
        for body in &bodies {
            for (tx, rx) in [(&mut ctx, &mut srx), (&mut stx, &mut crx)] {
                let n = tx
                    .seal(CONTENT_APPLICATION_DATA, body, &mut a)
                    .map_err(TlsError::Record)?;
                let room = rx.open_room(n);
                let got = rx
                    .open(&a[..n], &mut out[..room])
                    .map_err(TlsError::Record)?;
                ok &=
                    got == (CONTENT_APPLICATION_DATA, body.len()) && out[..body.len()] == body[..];
            }
        }
        Ok(ok)
    })?;
    assert!(ok, "{cp:?} client, {sp:?} server");
    Ok(())
}

#[test]
fn every_provider_completes_a_handshake_with_every_other_without_allocating_outside_the_exception()
{
    let id = identity();
    for cp in compiled() {
        for sp in compiled() {
            connect(&id, cp, sp).unwrap();
        }
    }
}

#[test]
fn a_provider_that_is_not_compiled_in_is_refused() {
    let id = identity();
    for p in Provider::ALL.into_iter().filter(|p| !p.compiled()) {
        assert!(
            ClientSetup::with_roots_using(p, &[&id.cert]).is_err(),
            "{p:?}"
        );
        assert!(
            ServerSetup::new_using(p, &[&id.cert], &id.key).is_err(),
            "{p:?}"
        );
        let mut d = Direction::new();
        assert_eq!(
            d.set_keys_with(p, Suite::Aes128Gcm, &[0; 16], &IV, 0),
            Err(RecordError::BadKey)
        );
    }
    assert!(Provider::preferred().compiled());
}
