// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios (`T3-21`–`T3-30`): `longshore_mailbox` integration
//! with `Posix` wakers, 2-core `Engine<Virtual>` trace parity, `submit_chain`,
//! `power_loss`, `longshore_s3::client`, armed `Accept`/`Recv`, `Stream`, Unix sockets, and WAL.

pub mod part1_posix_sim;
pub mod part2_storage_s3;
pub mod part3_client_armed;
pub mod part4_mux_unix_wal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PingMsg {
    pub seq: u64,
    pub slot: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WriteCmd {
    pub slot: u32,
    pub file_off: u64,
    pub len: u32,
    pub fill: u8,
}
