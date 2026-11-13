// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Reading the cache: lookups take no `Io`, so the request path cannot
//! block on name resolution.

use super::Resolver;
use std::net::IpAddr;

impl Resolver {
    /// True once every hostname has an address in some family.
    pub fn ready(&self) -> bool {
        self.hosts.iter().all(|h| h.sets[h.current()].len > 0)
    }

    pub fn host_index(&self, name: &str) -> Option<usize> {
        fn bare(n: &str) -> &[u8] {
            n.strip_suffix('.').unwrap_or(n).as_bytes()
        }
        let name = bare(name);
        self.hosts
            .iter()
            .position(|h| bare(&h.name).eq_ignore_ascii_case(name))
    }

    /// Next address for a host, rotating through the records of the family
    /// in use. No I/O.
    pub fn lookup(&mut self, host: usize) -> Option<IpAddr> {
        let h = &mut self.hosts[host];
        let set = &mut h.sets[h.current()];
        if set.len == 0 {
            return None;
        }
        set.next = (set.next + 1) % set.len;
        Some(set.addrs[set.next])
    }

    /// The cached addresses lookups are handing out for a host: those of
    /// the family in use.
    pub fn addrs(&self, host: usize) -> &[IpAddr] {
        let h = &self.hosts[host];
        let set = &h.sets[h.current()];
        &set.addrs[..set.len]
    }

    /// A connection to `ip`, an address of `host`, could not be opened. If
    /// that address is of the family in use and the other family has
    /// addresses, lookups move to the other family. They stay there until a
    /// connection to it fails in turn: the resolver does not probe the
    /// preferred family again on its own.
    pub fn connect_failed(&mut self, host: usize, ip: IpAddr) {
        let h = &mut self.hosts[host];
        let failed = usize::from(ip.is_ipv6());
        if failed == h.current() && h.sets[1 - failed].len > 0 {
            h.active = 1 - failed;
            self.family_switches += 1;
        }
    }
}
