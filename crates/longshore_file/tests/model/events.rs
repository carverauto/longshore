// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Each event judged against the ticket's record: it names a ticket in
//! flight, once (F2), and the file it is of; a sequence's steps report in
//! order and after a failed one `Cancelled` (F5); the set lets go of its
//! extents at the report (F3); a read returns what the model says (F7);
//! failed syncs and durable writes poison the file (C10); and the model's
//! bytes, names and handles follow.

use super::run::Run;
use super::state::{Pending, Target, What};
use longshore_common::io::Io;
use longshore_file::{ErrorKind, Event, FileError};

impl Run {
    pub fn on_event(&mut self, e: Event) {
        let Event::Done {
            ticket,
            file,
            result,
        } = e;
        let at = self.st.pending.iter().position(|p| p.ticket == ticket);
        let Some(at) = at else {
            panic!("F2: {e:?} names no ticket in flight");
        };
        let p = self.st.pending.remove(at);
        self.note(format_args!("event {result:?} of {ticket:?}"));
        let opened = matches!(p.what, What::Open { .. });
        let path = p.of_path();
        match (opened, path) {
            (true, _) if result.is_ok() => assert!(file.is_some(), "an open's file"),
            (true, _) => assert_eq!(file, None, "a failed open names no file"),
            (false, true) => assert_eq!(file, None, "a path operation's file"),
            (false, false) => assert_eq!(file, p.file, "the file of {e:?}"),
        }
        self.in_order(&p, result);
        for x in &p.extents {
            let others = self
                .st
                .pending
                .iter()
                .filter(|q| q.extents.contains(x))
                .count();
            let want = self.st.ours(*x) as usize + others;
            let held = self.rig.x().holds(*x).map_or(0, usize::from);
            assert_eq!(held, want, "F3: holds of {x:?} at its report");
        }
        let now = self.st.tick();
        match result {
            Err(FileError::Cancelled) => self.stats.cancelled += 1,
            Err(_) => self.stats.failed += 1,
            Ok(_) => {}
        }
        self.apply(p, file, result, now);
    }

    /// F5: a sequence's steps report in step order; after a failed step
    /// every later one is `Cancelled`, and none is before.
    fn in_order(&mut self, p: &Pending, result: Result<u64, FileError>) {
        let Some((id, k)) = p.seq else { return };
        let s = &mut self.st.seqs[id];
        assert!(!s.reported[k], "F2: step {k} of sequence {id} twice");
        let first = s.reported[..k].iter().all(|r| *r);
        s.reported[k] = true;
        // A step that waits behind an earlier one ends `Poisoned` as soon
        // as its file is (C10), before the earlier one reports, and the
        // steps after it `Cancelled`: never started, they are the one way
        // out of order.
        let after = s.failed_at.is_some_and(|at| at < k);
        let unstarted =
            result == Err(FileError::Poisoned) || (after && result == Err(FileError::Cancelled));
        assert!(
            first || unstarted,
            "F5: step {k} of sequence {id} out of order: {result:?}"
        );
        if after {
            let ended = matches!(result, Err(FileError::Cancelled | FileError::Poisoned));
            assert!(
                ended,
                "F5: step {k} of {id} after a failed step: {result:?}"
            );
        } else {
            assert!(
                result != Err(FileError::Cancelled),
                "F5: cancelled, nothing failed"
            );
        }
        if result.is_err() && result != Err(FileError::Cancelled) {
            s.failed_at = Some(s.failed_at.map_or(k, |at| at.min(k)));
        }
    }

    /// Whether `e` is an outcome the campaign allows an operation of the
    /// kind `kind` that is not a refusal: what the disk can report, and
    /// `Poisoned` for one of a poisoned file.
    fn allowed(&self, kind: &str, e: FileError, poisoned: bool) -> bool {
        let eio = 5;
        let disk = self.campaign.faulty();
        let inner = |k: ErrorKind| {
            matches!(k, ErrorKind::Io { errno } if errno == eio)
                || k == ErrorKind::NoProgress
                || (k == ErrorKind::Poisoned && poisoned)
                || matches!(k, ErrorKind::SyncFailed { errno } if errno == eio)
        };
        match e {
            FileError::Cancelled => true,
            FileError::Poisoned => poisoned,
            FileError::Partial { kind: k, .. } => disk && inner(k),
            FileError::Io { errno } => disk && errno == eio,
            FileError::NoProgress => disk && kind == "write",
            FileError::SyncFailed { errno } => disk && errno == eio,
            FileError::NoSpace => disk && kind == "reserve",
            FileError::Unsupported => disk && kind == "punch",
            other @ (FileError::Full
            | FileError::Stale
            | FileError::Closing
            | FileError::NotOpen
            | FileError::Unaligned
            | FileError::TooLarge
            | FileError::BadPath
            | FileError::StaleExtent
            | FileError::TooManyHolds
            | FileError::NotFound
            | FileError::Exists
            | FileError::Denied
            | FileError::Invalid { .. }
            | FileError::Os { .. }) => panic!("{kind}: {other:?} is not an outcome a disk gives"),
        }
    }

    fn apply(
        &mut self,
        p: Pending,
        file: Option<longshore_file::File>,
        result: Result<u64, FileError>,
        now: u64,
    ) {
        let poisoned = self.poisoned_for(p.file, result);
        match p.what {
            What::Write {
                inode,
                off,
                data,
                dsync,
                pages,
            } => {
                for (i, busy) in self.pages_busy.iter_mut().enumerate() {
                    if pages & (1 << i) != 0 {
                        *busy = false;
                    }
                }
                self.wrote(p.file, (inode, off, &data, dsync), result, now);
            }
            What::Read { .. } => self.was_read(p, result, poisoned),
            What::Sync { target } => self.synced(p.file, target, p.accepted, result),
            What::Reserve {
                inode,
                off,
                len,
                keep,
            } => self.reserved((inode, off, len, keep), result, poisoned),
            What::Punch { inode, off, len } => self.punched((inode, off, len), result, poisoned),
            What::Open {
                target,
                dir,
                flags,
                expect,
            } => self.opened((target, dir, flags, expect), file, result),
            What::Close => self.closed(p.file, result),
            What::Status { expect, .. } => match (expect, result) {
                (_, Err(FileError::Cancelled)) => {}
                (Some(want), got) => assert_eq!(got, want, "status"),
                (None, got) => assert!(got.is_ok() || got == Err(FileError::NotFound)),
            },
            What::Rename { .. } | What::Unlink { .. } | What::Mkdir { .. } => {
                self.path_done(&p.what, result, now);
            }
        }
    }

    /// A reserve's event.
    fn reserved(
        &mut self,
        (inode, off, len, keep): (usize, u64, u64, bool),
        result: Result<u64, FileError>,
        poisoned: bool,
    ) {
        let ino = &mut self.st.inodes[inode];
        let (lo, hi) = (off as usize, (off + len) as usize);
        ino.end(lo, hi, |_| Some(None), None, false);
        match result {
            Ok(0) if !keep => ino.size = ino.size.map(|s| s.max(off + len)),
            Ok(0) => {}
            Err(e) if self.allowed("reserve", e, poisoned) => {
                if !keep {
                    self.st.inodes[inode].size = None;
                }
            }
            other => panic!("reserve: {other:?}"),
        }
    }

    /// A punch's event: zeros where it went, unknown where it failed.
    fn punched(
        &mut self,
        (inode, off, len): (usize, u64, u64),
        result: Result<u64, FileError>,
        poisoned: bool,
    ) {
        let put: Option<Option<u8>> = match result {
            Ok(0) => Some(Some(0)),
            Err(FileError::Unsupported | FileError::Cancelled) => Some(None),
            Err(_) | Ok(_) => None,
        };
        if let Err(e) = result {
            assert!(self.allowed("punch", e, poisoned), "punch: {e:?}");
        }
        assert!(matches!(result, Ok(0) | Err(_)), "punch: {result:?}");
        let ino = &mut self.st.inodes[inode];
        ino.end(off as usize, (off + len) as usize, |_| put, None, false);
    }

    /// A write's event: of `inode` at `off`, `data`, on a durable-write
    /// file or not.
    fn wrote(
        &mut self,
        f: Option<longshore_file::File>,
        (inode, off, data, dsync): (usize, u64, &[u8], bool),
        result: Result<u64, FileError>,
        now: u64,
    ) {
        let (lo, hi) = (off as usize, off as usize + data.len());
        let ino = &mut self.st.inodes[inode];
        match result {
            Ok(n) => {
                assert_eq!(n, data.len() as u64, "a write is whole or an error");
                ino.end(lo, hi, |b| Some(Some(data[b - lo])), Some(now), dsync);
                ino.size = ino.size.map(|s| s.max(hi as u64));
            }
            Err(e) => {
                // A write that ends in an error leaves its range past
                // `done` unknown; only a cancelled step was not run (C7).
                let untouched = e == FileError::Cancelled;
                ino.end(
                    lo,
                    hi,
                    |_| if untouched { Some(None) } else { None },
                    None,
                    false,
                );
                if !untouched && ino.size.is_some_and(|s| s < hi as u64) {
                    ino.size = None;
                }
                if let FileError::Partial { done, .. } = e {
                    assert!(done > 0 && done < data.len() as u64, "{e:?}");
                }
                let poisoned = self.poisoned_for(f, Err(e));
                assert!(self.allowed("write", e, poisoned), "write: {e:?}");
                // From the disk, not the set's own conclusion (NoProgress).
                let disk = matches!(
                    e,
                    FileError::Io { .. }
                        | FileError::SyncFailed { .. }
                        | FileError::Partial {
                            kind: ErrorKind::Io { .. } | ErrorKind::SyncFailed { .. },
                            ..
                        }
                );
                // On a durable-write file EIO is a failed sync (C10).
                let plain_eio = matches!(
                    e,
                    FileError::Io { errno: 5 }
                        | FileError::Partial {
                            kind: ErrorKind::Io { errno: 5 },
                            ..
                        }
                );
                assert!(
                    !(dsync && plain_eio),
                    "a durable write's EIO not a failed sync: {e:?}"
                );
                if dsync && disk {
                    // A failed durable write is a failed sync (C10).
                    self.st.inodes[inode].sync_failed();
                    self.poison(f);
                } else {
                    assert!(
                        !matches!(e, FileError::SyncFailed { .. }),
                        "a buffered write failed as a sync"
                    );
                }
            }
        }
    }

    fn was_read(&mut self, p: Pending, result: Result<u64, FileError>, poisoned: bool) {
        let What::Read {
            inode,
            off,
            len,
            extent,
            at,
            snap,
            sure,
            size_stable,
        } = p.what
        else {
            return;
        };
        match result {
            Ok(n) => {
                assert!(n <= u64::from(len), "read {n} of {len}");
                let size = self.st.inodes[inode].size;
                if let (true, true, Some(size)) = (size_stable, self.campaign.checks_bytes(), size)
                {
                    let want = u64::from(len).min(size.saturating_sub(off));
                    assert_eq!(n, want, "F7: a read is whole but at the end of the file");
                }
                if self.campaign.checks_bytes() {
                    let got = &self.rig.io().buf(extent.buf())[at as usize..][..n as usize];
                    for (k, b) in got.iter().enumerate().take(snap.len()) {
                        if sure[k] {
                            assert_eq!(*b, snap[k], "F7: byte {} of a read", off + k as u64);
                        }
                    }
                    self.stats.read_bytes += n;
                }
            }
            Err(e) => assert!(self.allowed("read", e, poisoned), "read: {e:?}"),
        }
        self.release(extent);
    }

    fn synced(
        &mut self,
        f: Option<longshore_file::File>,
        target: Target,
        accepted: u64,
        result: Result<u64, FileError>,
    ) {
        // A sync taken after its file was poisoned is refused; one that
        // waited ends Poisoned: none succeeds (C10, F6).
        let after_poison = f
            .and_then(|f| self.st.handle(f))
            .is_some_and(|h| h.poisoned && h.poisoned_at < accepted);
        assert!(
            result.is_err() || !after_poison,
            "a sync of a poisoned file succeeded"
        );
        match (result, target) {
            (Ok(0), Target::Inode(i)) => self.st.inodes[i].synced(accepted),
            (Ok(0), dir) => {
                let in_sub = dir == Target::Sub;
                for n in &mut self.st.names {
                    n.dir_synced(usize::from(in_sub), accepted);
                }
                let sub = &mut self.st.sub;
                if !in_sub && sub.made && sub.changed < accepted {
                    sub.sure = true;
                }
            }
            (Err(FileError::SyncFailed { errno: 5 }), t) => {
                if let Target::Inode(i) = t {
                    self.st.inodes[i].sync_failed();
                } else {
                    // Nothing it was to make durable is promised now.
                    let now = self.st.clock;
                    let dirs = [t == Target::Root, t == Target::Sub];
                    for n in &mut self.st.names {
                        if n.need != [0, 0] || n.sure.is_some() {
                            n.change(now, dirs);
                        }
                    }
                }
                assert!(self.campaign.faulty(), "a sync failed on a good disk");
                self.poison(f);
            }
            (Err(e), _) => {
                let p = self.poisoned_for(f, result);
                assert!(self.allowed("sync", e, p), "sync: {e:?}");
            }
            (other, _) => panic!("sync: {other:?}"),
        }
    }
}
