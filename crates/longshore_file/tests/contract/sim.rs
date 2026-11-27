// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The contract suite on the simulator, and what the simulator's own
//! checks share: a set on a simulated disk, and faults placed on the next
//! operation the set submits.

use longshore_common::io::Limits;
use longshore_common::sim::{DiskConfig, Faults, PlacedFault, SimIo};
use longshore_file::check::{self, Rig};

/// A disk of 512-byte sectors, room for every check's files.
pub fn disk() -> DiskConfig {
    DiskConfig::BASE
        .with_max_files(64)
        .with_max_dirs(8)
        .with_max_bytes_per_file(1 << 20)
        .with_max_name_len(1024)
        .with_write_log_entries(256)
        .with_write_log_bytes(1 << 20)
        .with_read_error_sectors(16)
        .with_sector_size(512)
}

/// A simulator for `limits` with [`disk`].
pub fn sim(limits: Limits) -> SimIo {
    let mut s = SimIo::new(limits, 7, Faults::NONE);
    s.setup_disk(disk());
    s
}

/// A set of the checks' sizes on a simulated disk.
pub fn rig() -> Rig<SimIo> {
    Rig::new(&mut sim, check::limits(), check::sizes())
}

/// Place `fault` on the `ahead`-th operation the set submits from now on
/// (1 for the next). Every operation the rig's backend is given is a file
/// operation, numbered by the simulator as it is given.
pub fn place(rig: &mut Rig<SimIo>, ahead: u64, fault: PlacedFault) {
    let n = rig.io().submitted() + ahead;
    rig.io().inner_mut().place_disk_fault(n, fault);
}

#[test]
fn the_contract_holds_on_the_simulator() {
    check::run_all(&mut sim, "/");
}
