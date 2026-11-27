// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The file driven through `longshore_common`'s raw operations, as a
//! program does without `longshore::file`: its own slot, tokens and
//! buffers. It does not resume a short transfer; it stops on one.

use crate::die::{die, OrDie};
use crate::opts::{Api, Opts};
use crate::pass::Pass;
use longshore::io::{BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};

/// The raw path's state: its file slot, and a buffer and a token for
/// each operation in flight (token `i` is buffer `i`).
pub(crate) struct Raw {
    slot: FileSlot,
    path_buf: BufId,
    path_len: u32,
    bufs: Box<[BufId]>,
    free: Vec<u32>,
    started: Box<[u64]>,
    /// The token of a sync, and of an open, a reserve or a close.
    sync: u64,
    control: u64,
}

impl Raw {
    /// The raw path over `bufs`, with file slot 0 and small buffer 0 of
    /// `limits`, which the set of `longshore::file` does not use.
    pub(crate) fn new<IO: Io>(io: &mut IO, o: &Opts, limits: &Limits, bufs: &[BufId]) -> Raw {
        let path = format!("{}\0", o.path);
        let path_buf = limits.small(0);
        let into = io.buf(path_buf);
        if path.len() > into.len() {
            die("--path is longer than a small buffer");
        }
        into[..path.len()].copy_from_slice(path.as_bytes());
        let depth = bufs.len() as u64;
        Raw {
            slot: FileSlot::new(0),
            path_buf,
            path_len: path.len() as u32,
            bufs: bufs.into(),
            free: Vec::with_capacity(bufs.len()),
            started: vec![0; bufs.len()].into_boxed_slice(),
            sync: depth,
            control: depth + 1,
        }
    }

    /// Make the file, reserve its size, and write it once, unmeasured:
    /// the measured writes then overwrite written blocks.
    pub(crate) fn prepare<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, o: &Opts) {
        let flags = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true)
            .with_empty(true)
            .with_direct(o.direct);
        self.open(io, out, flags);
        let reserve = Op::FileReserve {
            slot: self.slot,
            file_off: 0,
            len: o.file_size,
            keep_size: false,
        };
        // Best effort on macOS; a refusal elsewhere is reported.
        let r = self.one(io, out, reserve);
        if r != 0 && !cfg!(target_os = "macos") {
            die(format_args!("reserve: errno {}", -r));
        }
        let mut fill = Pass::new(o, true, true, 1);
        self.pass(io, out, &mut fill);
        self.close(io, out);
    }

    /// A measured run of every pass `o` asks for, each reported.
    pub(crate) fn run<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, o: &Opts, run: u32) {
        let flags = OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_direct(o.direct)
            .with_dsync(o.dsync);
        self.open(io, out, flags);
        for &write in o.passes() {
            let mut p = Pass::new(o, write, false, crate::seed(run, write));
            self.pass(io, out, &mut p);
            p.report(o, Api::Raw, run);
        }
        self.close(io, out);
    }

    fn open<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, flags: OpenFlags) {
        let open = Op::FileOpen {
            slot: self.slot,
            path_buf: self.path_buf,
            path_off: 0,
            path_len: self.path_len,
            flags,
        };
        let r = self.one(io, out, open);
        if r != 0 {
            die(format_args!("open: errno {}", -r));
        }
    }

    fn close<IO: Io>(&mut self, io: &mut IO, out: &mut Completions) {
        let r = self.one(io, out, Op::FileClose { slot: self.slot });
        if r != 0 {
            die(format_args!("close: errno {}", -r));
        }
    }

    /// Submit `op` alone and wait for its result.
    fn one<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, op: Op) -> i32 {
        io.submit(Token::new(self.control), op).or_die("submit");
        loop {
            io.tick(true, out).or_die("tick");
            if let Some(c) = out.pop() {
                if c.token().get() != self.control {
                    die("a completion of another operation");
                }
                return c.result();
            }
        }
    }

    /// One pass, its syncs included.
    fn pass<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, p: &mut Pass) {
        self.free.clear();
        self.free.extend((0..self.bufs.len() as u32).rev());
        let mut sync_at = None;
        loop {
            self.issue(io, p);
            if self.free.len() == self.bufs.len() && sync_at.is_none() {
                break;
            }
            io.tick(true, out).or_die("tick");
            while let Some(c) = out.pop() {
                self.complete(p, c, &mut sync_at);
            }
            if sync_at.is_none() && p.sync_due() {
                self.submit_sync(io);
                sync_at = Some(p.now());
            }
        }
        if p.final_sync() {
            let started = p.now();
            let sync = Op::FileSync {
                slot: self.slot,
                datasync: true,
            };
            let r = self.one(io, out, sync);
            if r != 0 {
                die(format_args!("sync: errno {}", -r));
            }
            p.sync_done(started);
        }
    }

    /// Submit an operation from every free buffer while the pass has some.
    fn issue<IO: Io>(&mut self, io: &mut IO, p: &mut Pass) {
        while let Some(&i) = self.free.last() {
            let Some(file_off) = p.next() else { break };
            let _: Option<u32> = self.free.pop();
            let (slot, buf, len) = (self.slot, self.bufs[i as usize], p.len());
            let op = if p.write {
                Op::FileWrite {
                    slot,
                    buf,
                    off: 0,
                    len,
                    file_off,
                }
            } else {
                Op::FileRead {
                    slot,
                    buf,
                    off: 0,
                    len,
                    file_off,
                }
            };
            self.started[i as usize] = p.now();
            io.submit(Token::new(u64::from(i)), op).or_die("submit");
        }
    }

    fn submit_sync<IO: Io>(&self, io: &mut IO) {
        let sync = Op::FileSync {
            slot: self.slot,
            datasync: true,
        };
        io.submit(Token::new(self.sync), sync).or_die("submit");
    }

    /// Take the completion `c`.
    fn complete(&mut self, p: &mut Pass, c: Completion, sync_at: &mut Option<u64>) {
        let token = c.token().get();
        if token == self.sync {
            if c.result() != 0 {
                die(format_args!("sync: errno {}", -c.result()));
            }
            p.sync_done(sync_at.take().or_die("a sync in flight"));
            return;
        }
        let i = u32::try_from(token)
            .ok()
            .filter(|&i| (i as usize) < self.bufs.len())
            .or_die("a completion of another operation");
        match u32::try_from(c.result()) {
            Ok(n) if n == p.len() => {}
            Ok(n) => die(format_args!("a transfer of {n} bytes of {}", p.len())),
            Err(_) => die(format_args!("a write or read: errno {}", -c.result())),
        }
        p.op_done(self.started[i as usize]);
        self.free.push(i);
    }
}
