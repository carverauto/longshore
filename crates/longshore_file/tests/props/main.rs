// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Property tests (Hegel) of the set (task 3.4): the plan refuses what C2
//! says for any sizes and adds the counts C2's table says otherwise, and
//! `ops_per_file` never lets one file take every slot, on the plan and on
//! a set built from it; transfers cut short any number of times end
//! whole; direct-I/O alignment is checked exactly as C9 says.

#[cfg(test)]
mod plan;

#[cfg(test)]
mod transfers;

#[cfg(test)]
mod common {
    use hegel::generators as gs;
    use hegel::TestCase;
    use longshore_common::io::Limits;
    use longshore_common::sim::{DiskConfig, Faults, SimIo};

    pub fn between(tc: &TestCase, low: u32, high: u32) -> u32 {
        tc.draw(gs::integers::<u32>().min_value(low).max_value(high))
    }

    /// A simulator of `limits` on a disk of `sector`-byte sectors.
    pub fn sim(limits: Limits, sector: u32) -> SimIo {
        let mut s = SimIo::new(limits, 11, Faults::NONE);
        s.setup_disk(
            DiskConfig::BASE
                .with_max_files(16)
                .with_max_dirs(4)
                .with_max_bytes_per_file(1 << 20)
                .with_max_name_len(256)
                .with_write_log_entries(256)
                .with_write_log_bytes(1 << 20)
                .with_read_error_sectors(8)
                .with_sector_size(sector),
        );
        s
    }
}
