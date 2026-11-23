// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios (`T3-11`–`T3-20`): armed networking, buffer rings,
//! `Posix` / `UringIo` / `SimIo` parity, Unix sockets, socket options, and `Stream` TLS/plain.

pub mod part1_armed_eof;
pub mod part2_client_bufs_unix;
pub mod part3_stream_plain;
pub mod part4_faults_replay;

use longshore::io::Limits;

pub(super) fn net_limits(
    sockets_max: u32,
    accept_start: u32,
    accept_len: u32,
    ring_buffers: u32,
    ring_buffer_size: u32,
) -> Limits {
    Limits::BASE
        .with_sockets_max(sockets_max)
        .with_buffers(4)
        .with_buffer_size(32 * 1024)
        .with_small_buffers(4)
        .with_small_buffer_size(4096)
        .with_ops_max(32)
        .with_accept_range_start(accept_start)
        .with_accept_range_len(accept_len)
        .with_ring_buffers(ring_buffers)
        .with_ring_buffer_size(ring_buffer_size)
}
