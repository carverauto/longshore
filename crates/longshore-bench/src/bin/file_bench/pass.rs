// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! One measured pass, whichever API drives it: the offsets it hands out,
//! when it stops, when a sync is due, and what it saw.

use crate::opts::{Api, Opts};
use std::time::Instant;

/// Buckets of [`Hist`]: 64 exact values, then 32 a power of two.
const BUCKETS: usize = 64 + 58 * 32;

/// Latencies in nanoseconds, kept to about 3 % in fixed memory.
pub(crate) struct Hist {
    counts: Box<[u64]>,
    n: u64,
    max: u64,
}

impl Hist {
    fn new() -> Hist {
        Hist {
            counts: vec![0; BUCKETS].into_boxed_slice(),
            n: 0,
            max: 0,
        }
    }

    fn bucket(v: u64) -> usize {
        if v < 64 {
            return v as usize;
        }
        let e = 63 - v.leading_zeros() as usize;
        64 + (e - 6) * 32 + ((v >> (e - 5)) & 31) as usize
    }

    /// The least value of bucket `i`.
    fn floor(i: usize) -> u64 {
        if i < 64 {
            return i as u64;
        }
        let (e, sub) = ((i - 64) / 32 + 6, ((i - 64) % 32) as u64);
        (32 + sub) << (e - 5)
    }

    fn add(&mut self, v: u64) {
        self.counts[Hist::bucket(v)] += 1;
        self.n += 1;
        self.max = self.max.max(v);
    }

    /// The value below which `per_mille` thousandths of the samples lie, in µs.
    fn us(&self, per_mille: u64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let rank = (self.n * per_mille).div_ceil(1000).clamp(1, self.n);
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Hist::floor(i).min(self.max) as f64 / 1000.0;
            }
        }
        self.max as f64 / 1000.0
    }
}

/// CPU time of the calling thread, in seconds.
fn thread_cpu() -> f64 {
    extern "C" {
        fn clock_gettime(clock: i32, ts: *mut [i64; 2]) -> i32;
    }
    #[cfg(target_os = "linux")]
    const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
    #[cfg(not(target_os = "linux"))]
    const CLOCK_THREAD_CPUTIME_ID: i32 = 16;
    let mut ts = [0i64; 2];
    // A failure leaves zero, which reads as none.
    let _: i32 = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts[0] as f64 + ts[1] as f64 / 1e9
}

/// When a pass stops handing out operations.
#[derive(Clone, Copy)]
enum Until {
    /// After this many bytes.
    Bytes(u64),
    /// After this many nanoseconds.
    Ns(u64),
}

/// A pass in progress.
pub(crate) struct Pass {
    /// A write pass, or a read pass.
    pub(crate) write: bool,
    len: u32,
    ops_in_file: u64,
    random: bool,
    rng: u64,
    until: Until,
    sync_every: u32,
    dsync: bool,
    issued: u64,
    done_ops: u64,
    since_sync: u32,
    ops: Hist,
    syncs: Hist,
    t0: Instant,
    cpu0: f64,
}

impl Pass {
    /// A pass as `o` asks, timed from now. The fill before the measured
    /// passes is `fill`: every block once, in order, with no sync but the
    /// last.
    pub(crate) fn new(o: &Opts, write: bool, fill: bool, seed: u64) -> Pass {
        let until = match (fill, o.secs) {
            (true, _) => Until::Bytes(o.file_size),
            (false, Some(s)) => Until::Ns(s * 1_000_000_000),
            (false, None) => Until::Bytes(o.bytes),
        };
        Pass {
            write,
            len: o.write,
            ops_in_file: o.file_size / u64::from(o.write),
            random: o.random && !fill,
            rng: seed | 1,
            until,
            sync_every: if fill { 0 } else { o.sync_every },
            dsync: o.dsync && !fill,
            issued: 0,
            done_ops: 0,
            since_sync: 0,
            ops: Hist::new(),
            syncs: Hist::new(),
            t0: Instant::now(),
            cpu0: thread_cpu(),
        }
    }

    /// Nanoseconds since the pass began.
    pub(crate) fn now(&self) -> u64 {
        self.t0.elapsed().as_nanos() as u64
    }

    /// The bytes of one operation.
    pub(crate) const fn len(&self) -> u32 {
        self.len
    }

    /// The file offset of the next operation, or `None` when the pass has
    /// handed out all it will.
    pub(crate) fn next(&mut self) -> Option<u64> {
        let more = match self.until {
            Until::Bytes(b) => self.issued * u64::from(self.len) < b,
            Until::Ns(ns) => self.now() < ns,
        };
        if !more {
            return None;
        }
        let at = if self.random {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            self.rng % self.ops_in_file
        } else {
            self.issued % self.ops_in_file
        };
        self.issued += 1;
        Some(at * u64::from(self.len))
    }

    /// An operation begun at `started` is done, whole.
    pub(crate) fn op_done(&mut self, started: u64) {
        let now = self.now();
        self.ops.add(now.saturating_sub(started));
        self.done_ops += 1;
        self.since_sync += 1;
    }

    /// Whether a sync is due now, with none in flight: if so, the writes
    /// done so far are counted as covered by it.
    pub(crate) fn sync_due(&mut self) -> bool {
        let due = self.write && self.sync_every != 0 && self.since_sync >= self.sync_every;
        if due {
            self.since_sync = 0;
        }
        due
    }

    /// Whether the pass ends with a data sync, inside its time: a write
    /// pass does, unless its writes were durable each.
    pub(crate) const fn final_sync(&self) -> bool {
        self.write && !self.dsync
    }

    /// A sync begun at `started` is done.
    pub(crate) fn sync_done(&mut self, started: u64) {
        let now = self.now();
        self.syncs.add(now.saturating_sub(started));
    }

    /// Print the pass as one JSON line.
    pub(crate) fn report(&self, o: &Opts, api: Api, run: u32) {
        let secs = self.t0.elapsed().as_secs_f64();
        let cpu = thread_cpu() - self.cpu0;
        let bytes = self.done_ops * u64::from(self.len);
        let gb = bytes as f64 / 1e9;
        println!(
            concat!(
                "{{\"api\":\"{}\",\"backend\":\"{}\",\"phase\":\"{}\",\"run\":{},",
                "\"direct\":{},\"dsync\":{},\"offsets\":\"{}\",\"block\":{},",
                "\"write_size\":{},\"depth\":{},\"sync_every\":{},\"file_size\":{},",
                "\"bytes\":{},\"ops\":{},\"secs\":{:.3},\"gb_per_s\":{:.3},",
                "\"ops_per_s\":{:.0},\"op_p50_us\":{:.1},\"op_p99_us\":{:.1},",
                "\"op_p999_us\":{:.1},\"op_max_us\":{:.1},\"syncs\":{},",
                "\"sync_p50_us\":{:.1},\"sync_p99_us\":{:.1},\"sync_max_us\":{:.1},",
                "\"thread_cpu\":{:.3},\"cpu_s_per_gb\":{:.4}}}"
            ),
            api.name(),
            o.backend.name(),
            if self.write { "write" } else { "read" },
            run,
            o.direct,
            o.dsync,
            if o.random { "random" } else { "seq" },
            o.block,
            o.write,
            o.depth,
            o.sync_every,
            o.file_size,
            bytes,
            self.done_ops,
            secs,
            gb / secs,
            self.done_ops as f64 / secs,
            self.ops.us(500),
            self.ops.us(990),
            self.ops.us(999),
            self.ops.max as f64 / 1000.0,
            self.syncs.n,
            self.syncs.us(500),
            self.syncs.us(990),
            self.syncs.max as f64 / 1000.0,
            cpu / secs,
            if gb > 0.0 { cpu / gb } else { 0.0 },
        );
    }
}
