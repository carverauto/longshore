// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `--tls FILE`: both ends of a run speak TLS 1.3 through the transport's
//! record layer, with the certificate in `FILE` (DER) and its key in
//! `FILE.key` (PKCS#8 DER). The accepting end makes both, for the name
//! [`NAME`], when it finds no `FILE`; the connecting end only reads `FILE`,
//! and fails if it is missing. On two machines the load is given a copy of
//! the peer's `FILE`.

use super::Opts;
use crate::die::{die, OrDie};
use longshore::tcp::Tls;
use longshore::tls::handshake::{ClientSetup, ServerSetup};

/// The name the certificate is made for, and that the load verifies.
pub(crate) const NAME: &str = "bench.test";

/// The key's file beside the certificate's.
fn key_path(cert: &str) -> String {
    format!("{cert}.key")
}

/// Make a certificate and its key at `cert`, unless the certificate is
/// there already.
fn make_if_missing(cert: &str) {
    if std::path::Path::new(cert).exists() {
        return;
    }
    let k = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).or_die("certificate");
    let write = |path: &str, bytes: &[u8]| {
        std::fs::write(path, bytes).unwrap_or_else(|e| die(format_args!("{path}: {e}")));
    };
    write(&key_path(cert), &k.signing_key.serialize_der());
    write(cert, k.cert.der());
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| die(format_args!("{path}: {e}")))
}

/// What the accepting end presents, if the run is over TLS.
pub(crate) fn server(o: &Opts) -> Option<Tls> {
    let cert = o.tls.as_deref()?;
    make_if_missing(cert);
    let (der, key) = (read(cert), read(&key_path(cert)));
    let setup = ServerSetup::new_using(o.provider, &[&der], &key).or_die("server setup");
    Some(Tls::new().with_server(setup))
}

/// What the connecting end trusts, if the run is over TLS: the certificate
/// the accepting end made, never one of its own.
pub(crate) fn client(o: &Opts) -> Option<Tls> {
    let cert = o.tls.as_deref()?;
    let setup = ClientSetup::with_roots_using(o.provider, &[&read(cert)]).or_die("client setup");
    Some(Tls::new().with_client(setup))
}

/// The provider sealing the records, or `-` for a run in the clear.
pub(crate) fn label(o: &Opts) -> &'static str {
    if o.tls.is_some() {
        o.provider.feature()
    } else {
        "-"
    }
}
