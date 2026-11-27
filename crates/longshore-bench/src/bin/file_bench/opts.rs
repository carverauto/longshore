// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! What a run of `file_bench` is asked to be.

use crate::die::{die, OrDie};

/// The backend the file is driven on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    /// `UringIo`, Linux only.
    Uring,
    /// `PosixIo`: `kqueue` on macOS, `epoll` on Linux.
    Posix,
}

impl Backend {
    /// Its name in the output.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Backend::Uring => "uring",
            Backend::Posix => "posix",
        }
    }
}

/// Which way the file is driven.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Api {
    /// Through `longshore::file::Files`.
    File,
    /// Through `Op::FileWrite`, `Op::FileRead` and `Op::FileSync`,
    /// submitted directly.
    Raw,
}

impl Api {
    /// Its name in the output.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Api::File => "file",
            Api::Raw => "raw",
        }
    }
}

/// What each run measures.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// A write pass.
    Write,
    /// A read pass.
    Read,
    /// A write pass, then a read pass.
    Both,
}

/// What a run is asked to be.
#[derive(Clone)]
pub(crate) struct Opts {
    pub(crate) backend: Backend,
    /// One API, or both in turn (`None`).
    pub(crate) api: Option<Api>,
    pub(crate) phase: Phase,
    /// The file is opened for direct I/O (`O_DIRECT`, `F_NOCACHE` on macOS).
    pub(crate) direct: bool,
    /// The block size of the pool and of direct I/O.
    pub(crate) block: u32,
    /// The bytes of one write or read.
    pub(crate) write: u32,
    /// Writes or reads in flight at once.
    pub(crate) depth: u32,
    /// A data sync after every this many writes; 0 for one at the end only.
    pub(crate) sync_every: u32,
    /// Every write durable when it completes (`OpenFlags::with_dsync`).
    pub(crate) dsync: bool,
    /// Offsets drawn at random, in whole operations, rather than in order.
    pub(crate) random: bool,
    /// The file's size: the offsets of a pass lie inside it.
    pub(crate) file_size: u64,
    /// Bytes a pass moves, when it is not bounded by time.
    pub(crate) bytes: u64,
    /// Seconds a pass runs, when it is bounded by time.
    pub(crate) secs: Option<u64>,
    /// The file. It is made, filled once, and removed at the end.
    pub(crate) path: String,
    /// Runs; each is a pass of every phase for every API asked for.
    pub(crate) runs: u32,
}

/// `value` as a count of bytes, with an optional `k`, `m` or `g` (powers of
/// 1,024).
fn bytes(flag: &str, value: &str) -> u64 {
    let (digits, shift) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 10),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 20),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 30),
        Some(_) | None => (value, 0),
    };
    let n: u64 = digits
        .parse()
        .ok()
        .or_die(&format!("{flag} takes a number, with k, m or g"));
    n.checked_mul(1 << shift)
        .or_die(&format!("{flag} is too large"))
}

/// `value` as a count that fits a `u32`.
fn count(flag: &str, value: &str) -> u32 {
    value.parse().ok().or_die(&format!("{flag} takes a number"))
}

/// `value` as bytes, as [`bytes`] reads them, that fit a `u32`.
fn small(flag: &str, value: &str) -> u32 {
    u32::try_from(bytes(flag, value))
        .ok()
        .or_die(&format!("{flag} is too large"))
}

impl Opts {
    fn new() -> Opts {
        Opts {
            backend: if cfg!(target_os = "linux") {
                Backend::Uring
            } else {
                Backend::Posix
            },
            api: None,
            phase: Phase::Both,
            direct: false,
            block: 4096,
            write: 65_536,
            depth: 16,
            sync_every: 0,
            dsync: false,
            random: false,
            file_size: 1 << 30,
            bytes: 0,
            secs: None,
            path: std::env::temp_dir()
                .join(format!("file_bench-{}.dat", std::process::id()))
                .to_string_lossy()
                .into_owned(),
            runs: 3,
        }
    }

    /// The command line, checked; exit status 1 with a message otherwise.
    pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Opts {
        let mut o = Opts::new();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--direct" => o.direct = true,
                "--dsync" => o.dsync = true,
                "--random" => o.random = true,
                // A flag given no value is refused for the empty one.
                _ => o.set(&flag, args.next().as_deref().unwrap_or_default()),
            }
        }
        if o.bytes == 0 {
            o.bytes = o.file_size;
        }
        o.check();
        o
    }

    /// Set what `flag` names to `value`.
    fn set(&mut self, flag: &str, value: &str) {
        match flag {
            "--backend" => {
                self.backend = match value {
                    "uring" => Backend::Uring,
                    "posix" => Backend::Posix,
                    _ => die("--backend takes uring or posix"),
                }
            }
            "--api" => {
                self.api = match value {
                    "file" => Some(Api::File),
                    "raw" => Some(Api::Raw),
                    "both" => None,
                    _ => die("--api takes file, raw or both"),
                }
            }
            "--phase" => {
                self.phase = match value {
                    "write" => Phase::Write,
                    "read" => Phase::Read,
                    "both" => Phase::Both,
                    _ => die("--phase takes write, read or both"),
                }
            }
            "--block" => self.block = small(flag, value),
            "--write-size" => self.write = small(flag, value),
            "--depth" => self.depth = count(flag, value),
            "--sync-every" => self.sync_every = count(flag, value),
            "--file-size" => self.file_size = bytes(flag, value),
            "--bytes" => self.bytes = bytes(flag, value),
            "--secs" => self.secs = Some(u64::from(count(flag, value))),
            "--path" => value.clone_into(&mut self.path),
            "--runs" => self.runs = count(flag, value),
            other => die(format_args!("unknown flag {other}")),
        }
    }

    /// Refuse what cannot be run.
    fn check(&self) {
        if !cfg!(target_os = "linux") && self.backend == Backend::Uring {
            die("--backend uring needs Linux");
        }
        if self.block < 4096 || !self.block.is_power_of_two() {
            die("--block takes a power of two of 4096 or more");
        }
        if self.write == 0 || !self.write.is_multiple_of(self.block) {
            die("--write-size takes whole blocks");
        }
        if self.depth == 0 || self.depth > 4096 || self.runs == 0 {
            die("--depth takes 1 to 4096, --runs 1 or more");
        }
        if self.file_size < u64::from(self.write)
            || !self.file_size.is_multiple_of(u64::from(self.write))
        {
            die("--file-size takes whole writes, one at least");
        }
        if self.secs == Some(0) || (self.secs.is_none() && self.bytes < u64::from(self.write)) {
            die("--secs takes 1 or more, --bytes one write at least");
        }
        if self.path.is_empty() {
            die("--path takes a path");
        }
        if self.dsync && self.sync_every != 0 {
            die("--dsync makes every write durable: --sync-every adds nothing to it");
        }
    }

    /// The APIs of run `run` (from 1), in the order they run.
    pub(crate) fn apis(&self, run: u32) -> &'static [Api] {
        match self.api {
            Some(Api::File) => &[Api::File],
            Some(Api::Raw) => &[Api::Raw],
            // Whichever runs second finds the file as the first left it,
            // so the order is swapped every run.
            None if run % 2 == 1 => &[Api::File, Api::Raw],
            None => &[Api::Raw, Api::File],
        }
    }

    /// The passes of a run: `true` for a write pass.
    pub(crate) fn passes(&self) -> &'static [bool] {
        match self.phase {
            Phase::Write => &[true],
            Phase::Read => &[false],
            Phase::Both => &[true, false],
        }
    }

    /// Whether the set of `longshore::file` is planned.
    pub(crate) fn with_set(&self) -> bool {
        self.api != Some(Api::Raw)
    }
}
