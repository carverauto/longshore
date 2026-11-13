// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Address families: which records the resolver asks for, and a host's
//! addresses per family.

use crate::dns::codec::{Record, ADDRS_MAX};
use std::net::IpAddr;

/// Which address families the resolver asks for, and which it prefers.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Family {
    /// A records only.
    V4,
    /// AAAA records only.
    V6,
    /// Both. Lookups give IPv6 addresses while there are any and connections
    /// to them work.
    PreferV6,
    /// Both. Lookups give IPv4 addresses while there are any and connections
    /// to them work.
    PreferV4,
}

impl Family {
    /// The records to ask for, the preferred family first.
    pub(super) fn records(self) -> &'static [Record] {
        match self {
            Family::V4 => &[Record::A],
            Family::V6 => &[Record::Aaaa],
            Family::PreferV6 => &[Record::Aaaa, Record::A],
            Family::PreferV4 => &[Record::A, Record::Aaaa],
        }
    }
}

/// Index of a record type's address set in a host.
pub(super) const fn set_of(record: Record) -> usize {
    match record {
        Record::A => 0,
        Record::Aaaa => 1,
    }
}

#[derive(Clone, Copy)]
pub(super) struct Set {
    pub(super) addrs: [IpAddr; ADDRS_MAX],
    /// The maximum segment size last seen on a connection to each address,
    /// or 0 if none has been measured.
    pub(super) sizes: [u32; ADDRS_MAX],
    pub(super) len: usize,
    pub(super) next: usize,
}

impl Set {
    pub(super) const EMPTY: Set = Set {
        addrs: [IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED); ADDRS_MAX],
        sizes: [0; ADDRS_MAX],
        len: 0,
        next: 0,
    };

    /// Replace the addresses with a new answer. An address that is still
    /// there keeps its measured size, and the others are forgotten.
    pub(super) fn replace(&mut self, addrs: &[IpAddr]) {
        let old = *self;
        self.len = addrs.len();
        self.next %= self.len.max(1);
        for (i, a) in addrs.iter().enumerate() {
            self.addrs[i] = *a;
            self.sizes[i] = old.addrs[..old.len]
                .iter()
                .position(|o| o == a)
                .map_or(0, |k| old.sizes[k]);
        }
    }

    /// The size an address needs to count as having large segments: half
    /// the largest seen, or a quarter of what the path could carry if that
    /// is more. 0 when nothing is known.
    fn target(&self, path_limit: u32) -> u32 {
        let largest = self.sizes[..self.len].iter().copied().max().unwrap_or(0);
        largest.div_ceil(2).max(path_limit / 4)
    }

    fn unmeasured(&self) -> usize {
        self.sizes[..self.len].iter().filter(|s| **s == 0).count()
    }

    /// The smallest segment size a connection should have, keeping at least
    /// `keep` addresses in use: connections measured below it are worth
    /// replacing. 0 when no address is known to have large segments, since
    /// there is then nowhere better to go.
    pub(super) fn floor(&self, keep: usize, path_limit: u32) -> u32 {
        let target = self.target(path_limit);
        let sizes = &self.sizes[..self.len];
        if target == 0 || !sizes.iter().any(|s| *s >= target) {
            return 0;
        }
        // Unmeasured addresses are handed out whatever the floor is, so
        // they count as kept.
        let mut order = [u32::MAX; ADDRS_MAX];
        for (o, s) in order.iter_mut().zip(sizes) {
            *o = if *s == 0 { u32::MAX } else { *s };
        }
        order[..self.len].sort_unstable_by(|a, b| b.cmp(a));
        target.min(order[keep.clamp(1, self.len) - 1])
    }

    /// The floor lookups apply. It is the same as [`Set::floor`], except
    /// that when every measured address is small for the path and others
    /// are unmeasured, the unmeasured ones are tried first.
    pub(super) fn lookup_floor(&self, keep: usize, path_limit: u32) -> u32 {
        match self.floor(keep, path_limit) {
            0 if self.unmeasured() > 0 => self.target(path_limit),
            floor => floor,
        }
    }

    /// Every address has been measured and all are small for the path: a
    /// new answer is the only way to find a better one.
    pub(super) fn starved(&self, path_limit: u32) -> bool {
        self.len > 0
            && path_limit > 0
            && self.unmeasured() == 0
            && self.sizes[..self.len].iter().all(|s| *s < path_limit / 4)
    }

    /// Addresses worth one new connection each: unmeasured ones, while no
    /// address is known to have large segments but some are known small.
    pub(super) fn probes(&self, keep: usize, path_limit: u32) -> usize {
        let measured = self.len - self.unmeasured();
        if measured > 0 && self.floor(keep, path_limit) == 0 && path_limit > 0 {
            self.unmeasured()
        } else {
            0
        }
    }
}

pub(super) struct Host {
    pub(super) name: String,
    /// Addresses per family: IPv4, then IPv6.
    pub(super) sets: [Set; 2],
    /// The set lookups read while it has addresses.
    pub(super) active: usize,
    /// The largest segment a path to this host could carry, as reported
    /// with connections to it, or 0.
    pub(super) path_limit: u32,
}

impl Host {
    /// The set to hand addresses out from: the active one, or the other
    /// when the active one is empty.
    pub(super) fn current(&self) -> usize {
        if self.sets[self.active].len > 0 {
            self.active
        } else {
            1 - self.active
        }
    }

    /// Apply a cycle's empty answers: a family that answered with no
    /// addresses is dropped if the other family has some. If every family
    /// answered empty, both keep their addresses and this returns true.
    pub(super) fn drop_empty(&mut self, empty: [bool; 2]) -> bool {
        if empty == [true; 2] {
            return true;
        }
        for set in 0..2 {
            if empty[set] && self.sets[1 - set].len > 0 {
                self.sets[set].len = 0;
                self.sets[set].next = 0;
            }
        }
        false
    }
}
