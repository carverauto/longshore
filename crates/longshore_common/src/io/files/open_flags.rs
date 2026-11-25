// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Getters and `with_*` setters for [`OpenFlags`].

use super::OpenFlags;

impl OpenFlags {
    /// No flag set: what `OpenFlags::default()` gives, usable in `const` items.
    pub const NONE: Self = Self {
        read: false,
        write: false,
        create: false,
        exclusive: false,
        empty: false,
        direct: false,
        directory: false,
        dsync: false,
    };

    /// Open for reading.
    #[inline]
    pub const fn read(self) -> bool {
        self.read
    }

    /// This value with [`OpenFlags::read`] set to `read`.
    #[inline]
    pub const fn with_read(mut self, read: bool) -> Self {
        self.read = read;
        self
    }

    /// Open for writing.
    #[inline]
    pub const fn write(self) -> bool {
        self.write
    }

    /// This value with [`OpenFlags::write`] set to `write`.
    #[inline]
    pub const fn with_write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// Create the file if it does not exist.
    #[inline]
    pub const fn create(self) -> bool {
        self.create
    }

    /// This value with [`OpenFlags::create`] set to `create`.
    #[inline]
    pub const fn with_create(mut self, create: bool) -> Self {
        self.create = create;
        self
    }

    /// With `create`: fail if the file exists.
    #[inline]
    pub const fn exclusive(self) -> bool {
        self.exclusive
    }

    /// This value with [`OpenFlags::exclusive`] set to `exclusive`.
    #[inline]
    pub const fn with_exclusive(mut self, exclusive: bool) -> Self {
        self.exclusive = exclusive;
        self
    }

    /// Empty the file on open (`O_TRUNC`).
    #[inline]
    pub const fn empty(self) -> bool {
        self.empty
    }

    /// This value with [`OpenFlags::empty`] set to `empty`.
    #[inline]
    pub const fn with_empty(mut self, empty: bool) -> Self {
        self.empty = empty;
        self
    }

    /// Bypass the page cache (`O_DIRECT`; `F_NOCACHE` on macOS).
    #[inline]
    pub const fn direct(self) -> bool {
        self.direct
    }

    /// This value with [`OpenFlags::direct`] set to `direct`.
    #[inline]
    pub const fn with_direct(mut self, direct: bool) -> Self {
        self.direct = direct;
        self
    }

    /// Open only a directory (`O_DIRECTORY`).
    #[inline]
    pub const fn directory(self) -> bool {
        self.directory
    }

    /// This value with [`OpenFlags::directory`] set to `directory`.
    #[inline]
    pub const fn with_directory(mut self, directory: bool) -> Self {
        self.directory = directory;
        self
    }

    /// Every write completes only when its data, and the metadata needed to
    /// read it back, is on stable storage (`O_DSYNC`), with no `FileSync`.
    ///
    /// The file's own name is not covered: a newly created file's directory
    /// entry is durable only once its directory is synced. On macOS the
    /// kernel hands each write to the drive but does not flush the drive's
    /// own cache, as `F_FULLFSYNC` does for `FileSync`; a power loss there
    /// can still lose a completed write.
    ///
    /// The simulator's disk is stronger than a kernel here, in two ways. A
    /// durable write also makes durable whatever else of the same file was
    /// written and not yet synced, where a kernel syncs the range written.
    /// And after a power loss a durable write is whole or absent, never
    /// torn, where real media can hold part of one. A recovery that leans
    /// on either has to be checked on a file not opened so, where the
    /// simulator does lose and tear.
    #[inline]
    pub const fn dsync(self) -> bool {
        self.dsync
    }

    /// This value with [`OpenFlags::dsync`] set to `dsync`.
    #[inline]
    pub const fn with_dsync(mut self, dsync: bool) -> Self {
        self.dsync = dsync;
        self
    }
}
