// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Building a server: every connection, stream and object slot is made here.

use super::*;

impl Server {
    pub(super) fn build(
        cfg: Config,
        #[cfg(feature = "tls")] tls: Option<(ServerSetup, u32)>,
    ) -> Server {
        assert!(cfg.head_max <= cfg.buffer_size && cfg.head_max <= u32::from(u16::MAX));
        #[cfg(feature = "tls")]
        let streams = streams(&cfg, tls.as_ref().map(|t| t.1));
        #[cfg(not(feature = "tls"))]
        let streams = streams(&cfg);
        Server {
            cfg,
            conns: vec![Conn::FREE; cfg.connections_max as usize].into_boxed_slice(),
            streams,
            #[cfg(feature = "tls")]
            tls: tls.map(|t| t.0),
            objects: (0..cfg.objects_max)
                .map(|_| Object::empty(cfg.buffer_size))
                .collect(),
            uploads: [0; multipart::UPLOADS_MAX],
            uploads_begun: 0,
            erase_denied: "",
            list_only_under: "",
            bucket: "",
            script: [Behavior::Normal; SCRIPT_MAX],
            script_head: 0,
            script_len: 0,
            accepting: false,
            allowed_keys: [([0; 64], 0); 2],
            allowed_count: 0,
            session_token: ([0; 64], 0),
            verify: None,
            versions: None,
            encrypts: true,
            sha256: None,
            accepted: 0,
            requests: 0,
            closed: 0,
        }
    }
}

/// One stream per connection, each sealing TLS records in its own two
/// buffers from `first_tls_buf` when the server speaks TLS.
fn streams(cfg: &Config, #[cfg(feature = "tls")] first_tls_buf: Option<u32>) -> Box<[Stream]> {
    (0..cfg.connections_max)
        .map(|i| {
            let token = |kind| Token(cfg.token_base + u64::from(i) * 8 + kind);
            let s = Stream::new(Sock(cfg.first_sock + i), token(K_SEND), token(K_RECV));
            #[cfg(feature = "tls")]
            let s = match first_tls_buf {
                Some(first) => s.with_tls_buffers(
                    BufId(first + 2 * i),
                    BufId(first + 2 * i + 1),
                    cfg.buffer_size,
                    cfg.buffer_size,
                    u32::MAX,
                ),
                None => s,
            };
            s
        })
        .collect()
}
