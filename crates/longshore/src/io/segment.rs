// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a measured connect reports, packed into its completion's result.

/// The segment sizes of one connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
    /// The connection's maximum segment size in bytes: the smaller of what
    /// each end accepts.
    pub size: u32,
    /// The largest segment the local path could carry, from its MTU, or 0
    /// when unknown. A `size` far below this means the far end is what
    /// keeps segments small.
    pub path_limit: u32,
}

/// The path limit is carried in units of this many bytes.
const LIMIT_UNIT: u32 = 8;

impl Segment {
    /// The result a successful connect completes with. Sizes above 65,535
    /// are reported as 65,535.
    pub fn encode(self) -> i32 {
        let size = self.size.min(0xffff);
        let limit = self.path_limit.min(0xffff) / LIMIT_UNIT;
        (size | limit << 16) as i32
    }

    /// Read a successful connect's result. `None` when the connection was
    /// not measured.
    pub fn decode(result: i32) -> Option<Segment> {
        let r = u32::try_from(result).ok()?;
        (r & 0xffff != 0).then_some(Segment {
            size: r & 0xffff,
            path_limit: (r >> 16) * LIMIT_UNIT,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_round_trips_and_stays_positive() {
        for (size, path_limit) in [(1432, 8944), (8228, 8960), (32741, 65496), (536, 0)] {
            let r = Segment { size, path_limit }.encode();
            assert!(r > 0);
            assert_eq!(Segment::decode(r), Some(Segment { size, path_limit }));
        }
        // The limit is kept to the unit below, and large sizes are capped.
        let got = Segment::decode(
            Segment {
                size: 70_000,
                path_limit: 8961,
            }
            .encode(),
        );
        assert_eq!(
            got,
            Some(Segment {
                size: 65_535,
                path_limit: 8960
            })
        );
    }

    #[test]
    fn an_unmeasured_or_failed_connect_has_no_segment() {
        assert_eq!(Segment::decode(0), None);
        assert_eq!(Segment::decode(-111), None);
    }
}
