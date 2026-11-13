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
    pub(super) len: usize,
    pub(super) next: usize,
}

pub(super) struct Host {
    pub(super) name: String,
    /// Addresses per family: IPv4, then IPv6.
    pub(super) sets: [Set; 2],
    /// The set lookups read while it has addresses.
    pub(super) active: usize,
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
