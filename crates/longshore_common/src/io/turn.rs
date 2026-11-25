// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The order of one turn of a thread that drives storage and network I/O
//! from one backend.
//!
//! One `tick` submits everything queued (the turn's log writes, its sync
//! and its sends leave in one system call) and collects a batch of
//! completions. [`Turn::dispatch`] then hands the batch out by class:
//! storage first, so that what became durable can be answered in the same
//! turn and a sync completion never waits behind a burst of receives; then
//! network; then everything else (timers, wakes). Within a class
//! completions keep their arrival order. The batch is already in memory,
//! so the order costs one pass per class and needs no second ring.

use super::{Completion, Completions, Token};

/// Whose completion a token is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// File operations: writes, syncs and the rest.
    Storage,
    /// Sockets: accepts, receives, sends.
    Network,
    /// Timers, wakes and whatever else the owner submits.
    Other,
}

/// Token ranges for storage and network, fixed when the thread is built.
#[derive(Clone, Copy, Debug)]
pub struct Turn {
    storage: (u64, u64),
    network: (u64, u64),
}

impl Turn {
    /// Tokens `storage.0..storage.1` are storage's and `network.0..network.1`
    /// the network's (each end exclusive); the ranges must not overlap.
    pub const fn new(storage: (u64, u64), network: (u64, u64)) -> Turn {
        assert!(storage.0 <= storage.1 && network.0 <= network.1);
        assert!(
            storage.1 <= network.0 || network.1 <= storage.0,
            "storage and network token ranges overlap"
        );
        Turn { storage, network }
    }

    /// The class of `token`.
    #[inline]
    pub const fn class(&self, token: Token) -> Class {
        let t = token.0;
        if t >= self.storage.0 && t < self.storage.1 {
            Class::Storage
        } else if t >= self.network.0 && t < self.network.1 {
            Class::Network
        } else {
            Class::Other
        }
    }

    /// Hand every completion in `batch` to `f`: storage first, then
    /// network, then the rest, each class in arrival order. Leaves `batch`
    /// empty. Does not allocate.
    pub fn dispatch(&self, batch: &mut Completions, mut f: impl FnMut(Class, Completion)) {
        for class in [Class::Storage, Class::Network, Class::Other] {
            batch.drain_where(|c| self.class(c.token()) == class, |c| f(class, c));
        }
    }
}
