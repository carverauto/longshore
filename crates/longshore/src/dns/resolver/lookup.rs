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
    /// in use. With a preference for large segments, addresses measured
    /// below the floor are passed over. No I/O.
    pub fn lookup(&mut self, host: usize) -> Option<IpAddr> {
        let floor = self.segment_floor(host);
        let h = &mut self.hosts[host];
        let set = &mut h.sets[h.current()];
        // The floor always admits at least one address, so this ends.
        for _ in 0..set.len {
            set.next = (set.next + 1) % set.len;
            let size = set.sizes[set.next];
            if size == 0 || size >= floor {
                return Some(set.addrs[set.next]);
            }
        }
        None
    }

    /// A connection to `ip`, an address of `host`, opened with this maximum
    /// segment size. Returns the host's floor afterwards, as
    /// [`Resolver::segment_floor`] does.
    pub fn connected(&mut self, host: usize, ip: IpAddr, segment_size: u32) -> u32 {
        let set = &mut self.hosts[host].sets[usize::from(ip.is_ipv6())];
        if let Some(k) = set.addrs[..set.len].iter().position(|a| *a == ip) {
            set.sizes[k] = segment_size;
        }
        self.segment_floor(host)
    }

    /// Turn the preference for large segments on, keeping at least `keep`
    /// addresses in use, or off with `None`. Takes effect at the next
    /// lookup. Sizes already measured are kept either way. See
    /// [`super::Config::prefer_large_segments`].
    pub fn set_prefer_large_segments(&mut self, keep: Option<usize>) {
        self.cfg.prefer_large_segments = keep;
    }

    /// The smallest segment size a connection to `host` should have: lookups
    /// pass over addresses measured below it. 0 when there is no preference
    /// for large segments or nothing has been measured.
    pub fn segment_floor(&self, host: usize) -> u32 {
        let h = &self.hosts[host];
        match self.cfg.prefer_large_segments {
            Some(keep) => h.sets[h.current()].floor(keep),
            None => 0,
        }
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
