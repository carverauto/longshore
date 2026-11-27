// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Nothing is allocated after `Files::new` (task 3.3, F9): a run of every
//! operation, refusals, work that waits for room, a sequence, a held-back
//! close, power loss and `power_lost`, and the files opened again after
//! it, all under the allocation guard, on the simulator; and the same run
//! but power loss on the portable backend.

#[global_allocator]
static ALLOC: longshore_common::io::alloc_guard::Guard = longshore_common::io::alloc_guard::Guard;

#[cfg(test)]
mod run;

#[cfg(test)]
mod tests {
    use super::run::{every_operation, lose_power, Paths};
    use longshore_common::io::alloc_guard::sealed;
    use longshore_common::io::Limits;
    use longshore_common::sim::{DiskConfig, Faults, SimIo};
    use longshore_file::check::{self, Rig};

    fn sim(l: Limits) -> SimIo {
        let mut s = SimIo::new(l, 3, Faults::NONE);
        s.setup_disk(
            DiskConfig::BASE
                .with_max_files(32)
                .with_max_dirs(8)
                .with_max_bytes_per_file(1 << 20)
                .with_max_name_len(256)
                .with_write_log_entries(256)
                .with_write_log_bytes(1 << 20)
                .with_read_error_sectors(8)
                .with_sector_size(512),
        );
        s
    }

    #[test]
    fn nothing_is_allocated_after_build_on_the_simulator() {
        let base = check::limits().with_area_pages(2);
        let mut rig = Rig::new(&mut sim, base, check::sizes());
        let paths = Paths::under("/", rig.config().path_max());
        sealed(|| {
            every_operation(&mut rig, &paths, true);
            lose_power(&mut rig, &paths);
            // And again after the power loss, from files opened anew.
            every_operation(&mut rig, &paths, true);
        });
        rig.at_rest(0);
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_allocated_after_build_on_the_portable_backend() {
        use longshore_common::io::posix::PosixIo;
        let dir = std::env::temp_dir().join(format!("longshore_file_alloc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = format!("{}/", dir.display());
        let mut rig = Rig::new(
            &mut |l| PosixIo::new(l).unwrap(),
            check::limits(),
            check::sizes(),
        );
        let paths = Paths::under(&root, rig.config().path_max());
        sealed(|| every_operation(&mut rig, &paths, false));
        rig.at_rest(0);
        let _: Option<()> = std::fs::remove_dir_all(&dir).ok();
    }
}
