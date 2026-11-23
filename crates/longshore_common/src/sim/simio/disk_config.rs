// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters and `with_*` setters for [`DiskConfig`].

use super::DiskConfig;

impl DiskConfig {
    /// Files the disk can hold.
    #[inline]
    pub const fn max_files(self) -> u32 {
        self.max_files
    }

    /// This value with [`DiskConfig::max_files`] set to `max_files`.
    #[inline]
    pub const fn with_max_files(mut self, max_files: u32) -> Self {
        self.max_files = max_files;
        self
    }

    /// Directories the disk can hold.
    #[inline]
    pub const fn max_dirs(self) -> u32 {
        self.max_dirs
    }

    /// This value with [`DiskConfig::max_dirs`] set to `max_dirs`.
    #[inline]
    pub const fn with_max_dirs(mut self, max_dirs: u32) -> Self {
        self.max_dirs = max_dirs;
        self
    }

    /// Bytes each file can hold.
    #[inline]
    pub const fn max_bytes_per_file(self) -> usize {
        self.max_bytes_per_file
    }

    /// This value with [`DiskConfig::max_bytes_per_file`] set to `max_bytes_per_file`.
    #[inline]
    pub const fn with_max_bytes_per_file(mut self, max_bytes_per_file: usize) -> Self {
        self.max_bytes_per_file = max_bytes_per_file;
        self
    }

    /// Longest path component in bytes.
    #[inline]
    pub const fn max_name_len(self) -> usize {
        self.max_name_len
    }

    /// This value with [`DiskConfig::max_name_len`] set to `max_name_len`.
    #[inline]
    pub const fn with_max_name_len(mut self, max_name_len: usize) -> Self {
        self.max_name_len = max_name_len;
        self
    }

    /// Unsynced writes the disk remembers for crash recovery.
    #[inline]
    pub const fn write_log_entries(self) -> u32 {
        self.write_log_entries
    }

    /// This value with [`DiskConfig::write_log_entries`] set to `write_log_entries`.
    #[inline]
    pub const fn with_write_log_entries(mut self, write_log_entries: u32) -> Self {
        self.write_log_entries = write_log_entries;
        self
    }

    /// Bytes of unsynced writes the disk remembers.
    #[inline]
    pub const fn write_log_bytes(self) -> usize {
        self.write_log_bytes
    }

    /// This value with [`DiskConfig::write_log_bytes`] set to `write_log_bytes`.
    #[inline]
    pub const fn with_write_log_bytes(mut self, write_log_bytes: usize) -> Self {
        self.write_log_bytes = write_log_bytes;
        self
    }

    /// Bad sectors the disk can track at once.
    #[inline]
    pub const fn read_error_sectors(self) -> u32 {
        self.read_error_sectors
    }

    /// This value with [`DiskConfig::read_error_sectors`] set to `read_error_sectors`.
    #[inline]
    pub const fn with_read_error_sectors(mut self, read_error_sectors: u32) -> Self {
        self.read_error_sectors = read_error_sectors;
        self
    }

    /// Sector size in bytes: 512 or 4096.
    #[inline]
    pub const fn sector_size(self) -> u32 {
        self.sector_size
    }

    /// This value with [`DiskConfig::sector_size`] set to `sector_size`.
    #[inline]
    pub const fn with_sector_size(mut self, sector_size: u32) -> Self {
        self.sector_size = sector_size;
        self
    }
}
