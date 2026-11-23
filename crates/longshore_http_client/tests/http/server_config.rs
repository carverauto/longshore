// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The in-tree server's configuration: what the builder accepts, and what
//! it refuses before a server could fail on it.

use longshore_common::io::Sock;
use longshore_http_client::server::{ServerConfig, ServerConfigBuilder, ServerConfigError};

/// One connection on socket 1, listening on socket 0, with 4 KiB buffers.
#[cfg(test)]
fn one() -> ServerConfigBuilder {
    ServerConfig::builder(Sock::new(0), 1, 1, 0, 4096, 0)
}

#[test]
fn a_built_config_reads_back_what_it_was_given() {
    let sum = |_: &[u8]| [b'A'; 12];
    let cfg = ServerConfig::builder(Sock::new(9), 3, 10, 4, 8192, 2 << 40)
        .head_max(2048)
        .objects_max(5)
        .checksum(sum)
        .build()
        .unwrap();
    assert_eq!(
        (cfg.listener(), cfg.connections_max(), cfg.first_sock()),
        (Sock::new(9), 3, 10)
    );
    assert_eq!(
        (cfg.first_buf(), cfg.head_max(), cfg.buffer_size()),
        (4, 2048, 8192)
    );
    assert_eq!((cfg.objects_max(), cfg.token_base()), (5, 2 << 40));
    assert_eq!(cfg.checksum().map(|f| f(b"")), Some([b'A'; 12]));
}

#[test]
fn defaults_are_a_4096_byte_head_eight_objects_and_no_checksum() {
    let cfg = one().build().unwrap();
    assert_eq!((cfg.head_max(), cfg.objects_max()), (4096, 8));
    assert!(cfg.checksum().is_none());
}

#[test]
fn a_server_without_connections_is_refused() {
    let refused = ServerConfig::builder(Sock::new(0), 0, 1, 0, 4096, 0).build();
    assert_eq!(refused.unwrap_err(), ServerConfigError::NoConnections);
}

#[test]
fn a_head_too_small_for_the_servers_own_answers_or_too_long_to_parse_is_refused() {
    for head_max in [0, 1023, 65_536] {
        let refused = one().head_max(head_max).build().unwrap_err();
        assert!(
            matches!(refused, ServerConfigError::HeadMaxOutOfRange { .. }),
            "{head_max}: {refused}"
        );
    }
    assert!(one().head_max(1024).build().is_ok());
}

#[test]
fn a_head_larger_than_the_buffers_is_refused() {
    let refused = ServerConfig::builder(Sock::new(0), 1, 1, 0, 2048, 0)
        .head_max(4096)
        .build();
    assert_eq!(
        refused.unwrap_err(),
        ServerConfigError::HeadMaxAboveBufferSize {
            head_max: 4096,
            buffer_size: 2048
        }
    );
}

#[test]
fn slot_buffer_and_token_ranges_must_fit_their_numbers() {
    let socks = ServerConfig::builder(Sock::new(0), 2, u32::MAX - 1, 0, 4096, 0).build();
    assert!(matches!(
        socks,
        Err(ServerConfigError::SocketsOverflow { .. })
    ));
    let bufs = ServerConfig::builder(Sock::new(0), 2, 1, u32::MAX - 3, 4096, 0).build();
    assert!(matches!(
        bufs,
        Err(ServerConfigError::BuffersOverflow { .. })
    ));
    let tokens = ServerConfig::builder(Sock::new(0), 2, 1, 0, 4096, u64::MAX - 16).build();
    assert!(matches!(
        tokens,
        Err(ServerConfigError::TokensOverflow { .. })
    ));
    assert!(
        ServerConfig::builder(Sock::new(0), 2, 1, u32::MAX - 4, 4096, u64::MAX - 17)
            .build()
            .is_ok()
    );
}

#[test]
fn a_listener_among_the_connection_slots_is_refused() {
    let refused = ServerConfig::builder(Sock::new(3), 4, 1, 0, 4096, 0)
        .build()
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "listener slot 3 is inside the connection slots 1..5"
    );
    assert!(ServerConfig::builder(Sock::new(5), 4, 1, 0, 4096, 0)
        .build()
        .is_ok());
}
