// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The client configuration's builder: its defaults, and every limit it refuses.

use longshore_http_client::client::{
    ClientConfig, ClientConfigBuilder, ClientConfigError, HEAD_MAX_DEFAULT, MAX_AGE_NS_DEFAULT,
    MAX_REQUESTS_DEFAULT, REQUEST_TIMEOUT_NS_DEFAULT, TLS_BATCH_DEFAULT_MAX, TLS_RECORDS_MAX,
    ZERO_COPY_MIN_DEFAULT,
};

const BUF: u32 = 32 * 1024;

fn base() -> ClientConfigBuilder {
    ClientConfig::builder(4, 1, 0, BUF, 1 << 40)
}

#[test]
fn a_builder_with_only_the_required_values_takes_the_defaults() {
    let cfg = base().build().unwrap();
    assert_eq!(
        (cfg.connections_max(), cfg.first_sock(), cfg.first_buf()),
        (4, 1, 0)
    );
    assert_eq!((cfg.buffer_size(), cfg.token_base()), (BUF, 1 << 40));
    assert_eq!(cfg.head_max(), HEAD_MAX_DEFAULT);
    assert_eq!(cfg.max_age_ns(), MAX_AGE_NS_DEFAULT);
    assert_eq!(cfg.max_requests(), MAX_REQUESTS_DEFAULT);
    assert_eq!(cfg.request_timeout_ns(), REQUEST_TIMEOUT_NS_DEFAULT);
    assert_eq!(cfg.zero_copy_min(), ZERO_COPY_MIN_DEFAULT);
    assert_eq!(cfg.tls_records_max(), TLS_RECORDS_MAX);
    assert!(cfg.check_idle());
    // The default batch is the buffer size, up to its own limit.
    assert_eq!(cfg.tls_batch(), BUF);
    let large = ClientConfig::builder(1, 0, 0, 8 << 20, 0).build().unwrap();
    assert_eq!(large.tls_batch(), TLS_BATCH_DEFAULT_MAX);
}

#[test]
fn every_setter_reaches_its_getter() {
    let cfg = base()
        .head_max(512)
        .max_age_ns(7)
        .max_requests(3)
        .request_timeout_ns(11)
        .zero_copy_min(13)
        .tls_batch(17)
        .tls_records_max(19)
        .check_idle(false)
        .build()
        .unwrap();
    assert_eq!(
        (cfg.head_max(), cfg.max_age_ns(), cfg.max_requests()),
        (512, 7, 3)
    );
    assert_eq!(
        (
            cfg.request_timeout_ns(),
            cfg.zero_copy_min(),
            cfg.tls_batch()
        ),
        (11, 13, 17)
    );
    assert_eq!((cfg.tls_records_max(), cfg.check_idle()), (19, false));
}

#[test]
fn the_limits_at_their_edges_are_accepted() {
    let edge = ClientConfig::builder(
        u32::MAX / 3,
        0,
        0,
        65535,
        u64::MAX - 8 * u64::from(u32::MAX / 3),
    )
    .head_max(65535)
    .tls_records_max(TLS_RECORDS_MAX)
    .max_age_ns(1)
    .max_requests(1)
    .request_timeout_ns(1)
    .build();
    assert!(edge.is_ok(), "{edge:?}");
    assert!(ClientConfig::builder(1, u32::MAX - 1, u32::MAX - 1, 1, 0)
        .head_max(1)
        .tls_records_max(1)
        .build()
        .is_ok());
}

#[test]
fn a_configuration_the_client_cannot_run_with_is_refused() {
    use ClientConfigError as E;
    let refused = [
        (ClientConfig::builder(0, 1, 0, BUF, 0), E::Connections),
        (
            ClientConfig::builder(u32::MAX / 3 + 1, 0, 0, BUF, 0),
            E::Connections,
        ),
        (
            ClientConfig::builder(2, u32::MAX - 1, 0, BUF, 0),
            E::SocketRange,
        ),
        (
            ClientConfig::builder(2, 0, u32::MAX - 1, BUF, 0),
            E::BufferRange,
        ),
        (
            ClientConfig::builder(2, 0, 0, BUF, u64::MAX - 8),
            E::TokenRange,
        ),
        (base().head_max(0), E::HeadMax),
        (base().head_max(BUF + 1), E::HeadMax),
        (
            ClientConfig::builder(1, 0, 0, 1 << 20, 0).head_max(65536),
            E::HeadMax,
        ),
        (ClientConfig::builder(1, 0, 0, 4096, 0), E::HeadMax),
        (base().max_age_ns(0), E::MaxAge),
        (base().max_requests(0), E::MaxRequests),
        (base().request_timeout_ns(0), E::RequestTimeout),
        (base().tls_records_max(0), E::TlsRecordsMax),
        (
            base().tls_records_max(TLS_RECORDS_MAX + 1),
            E::TlsRecordsMax,
        ),
    ];
    for (builder, error) in refused {
        assert_eq!(builder.build(), Err(error), "{builder:?}");
        assert!(!error.to_string().is_empty());
    }
}
