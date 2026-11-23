// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The numbers that name tokens, buffers, registered files and sockets.

/// Caller-chosen identifier returned in the matching [`super::Completion`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token(pub(crate) u64);

impl Token {
    /// The token with this value.
    #[inline]
    pub const fn new(value: u64) -> Token {
        Token(value)
    }

    /// The value the token was made with.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Index into the buffer pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufId(pub(crate) u32);

impl BufId {
    /// Buffer `index` of the pool.
    #[inline]
    pub const fn new(index: u32) -> BufId {
        BufId(index)
    }

    /// The buffer's index in the pool.
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A file registered at initialization with [`super::Io::register_file`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileId(pub(crate) u32);

impl FileId {
    /// Registered file `index`.
    #[inline]
    pub const fn new(index: u32) -> FileId {
        FileId(index)
    }

    /// The file's index among the registered files.
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Index into the socket table. The caller assigns slots.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sock(pub(crate) u32);

impl Sock {
    /// Socket slot `index`.
    #[inline]
    pub const fn new(index: u32) -> Sock {
        Sock(index)
    }

    /// The slot's index in the socket table.
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }
}
