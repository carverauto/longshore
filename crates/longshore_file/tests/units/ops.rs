// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Each operation, and the handles and tickets they go by (contract C3 to
//! C5, C7, C9, C11).

use crate::rig::{rw, Rig, EXTENT};
use longshore_common::io::{Io, OpenFlags};
use longshore_file::{FileError, FileSizes, Span, Step, Ticket};

#[test]
fn reserve_and_punch() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/r.dat", rw());
    let t1 = r.files.reserve(&mut r.io, f, (0, 8192), false).unwrap();
    assert_eq!(r.wait(t1).1, Ok(0));
    let t2 = r.files.punch(&mut r.io, f, (0, 4096)).unwrap();
    assert_eq!(r.wait(t2).1, Ok(0));
    let t3 = r.files.status(&mut r.io, "/r.dat").unwrap();
    assert_eq!(r.wait(t3).1, Ok(8192));
}

#[test]
fn write_sync_read_back_close() {
    let mut r = Rig::new(FileSizes::new(2, 2));
    let f = r.open("/a.dat", rw());
    let w = r.extent(0x5A);
    assert_eq!(r.write(f, w, 8192, 4096), Ok(8192));
    assert_eq!(r.sync(f), Ok(0));
    let back = r.extent(0);
    assert_eq!(
        r.read(f, back, EXTENT, 4096),
        Ok(8192),
        "short only at the end"
    );
    assert!(r.io.buf(back.buf())[..8192].iter().all(|b| *b == 0x5A));
    r.close(f);
    let t = r.files.status(&mut r.io, "/a.dat").unwrap();
    assert_eq!(r.wait(t), (None, Ok(4096 + 8192)));
    // The set released its holds when each operation reported.
    assert_eq!(r.x.holds(w), Some(1));
    assert_eq!(r.x.holds(back), Some(1));
    r.settle();
    let u = r.files.usage();
    assert_eq!(
        (u.files(), u.ops(), u.waiting(), u.broken_holds()),
        (0, 0, 0, 0)
    );
}

#[test]
fn a_stale_handle_is_refused_and_never_reaches_the_new_file() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let old = r.open("/a.dat", rw());
    r.close(old);
    let new = r.open("/b.dat", rw());
    assert_eq!(old.slot(), new.slot());
    assert_ne!(old, new);
    let e = r.extent(1);
    let refused = r.files.write_at(&mut r.io, &mut r.x, old, (e, 0, 4096), 0);
    assert_eq!(refused, Err(FileError::Stale));
    assert_eq!(r.files.close(&mut r.io, old), Err(FileError::Stale));
    assert_eq!(r.files.sync(&mut r.io, old, true), Err(FileError::Stale));
    // A refusal takes no hold and no slot.
    assert_eq!(r.x.holds(e), Some(1));
    assert_eq!(r.files.usage().ops(), 0);
    // A closing file is refused too, and its close goes once it is alone.
    let w = r
        .files
        .write_at(&mut r.io, &mut r.x, new, (e, 0, 4096), 0)
        .unwrap();
    let c = r.files.close(&mut r.io, new).unwrap();
    assert_eq!(r.files.sync(&mut r.io, new, true), Err(FileError::Closing));
    assert_eq!(r.wait(w).1, Ok(4096));
    assert_eq!(r.wait(c), (Some(new), Ok(0)));
}

#[test]
fn refusals_at_the_call() {
    let mut r = Rig::new(FileSizes::new(2, 2));
    let made = r.open("/ro.dat", rw());
    r.close(made);
    let ro = r.open("/ro.dat", OpenFlags::NONE.with_read(true));
    let e = r.extent(0);
    let w = r.files.write_at(&mut r.io, &mut r.x, ro, (e, 0, 4096), 0);
    assert_eq!(w, Err(FileError::NotOpen));
    assert_eq!(
        r.files.reserve(&mut r.io, ro, (0, 4096), false),
        Err(FileError::NotOpen)
    );
    let f = r.open("/rw.dat", rw());
    let past = r
        .files
        .read_at(&mut r.io, &mut r.x, f, (e, 4096, EXTENT), 0);
    assert_eq!(past, Err(FileError::TooLarge));
    assert_eq!(
        r.files.reserve(&mut r.io, f, (0, 0), false),
        Err(FileError::TooLarge)
    );
    assert_eq!(
        r.files.punch(&mut r.io, f, (0, 100)),
        Err(FileError::Unaligned)
    );
    assert_eq!(r.files.open(&mut r.io, "", rw()), Err(FileError::BadPath));
    assert_eq!(r.files.unlink(&mut r.io, "/a\0b"), Err(FileError::BadPath));
    let long = "/".repeat(r.files.config().path_max() as usize + 1);
    assert_eq!(r.files.mkdir(&mut r.io, &long), Err(FileError::BadPath));
    // An extent freed since it was taken.
    let gone = r.extent(0);
    assert_eq!(r.x.release(gone), Ok(true));
    let stale = r.files.read_at(&mut r.io, &mut r.x, f, (gone, 0, 4096), 0);
    assert_eq!(stale, Err(FileError::StaleExtent));
    assert_eq!(r.files.usage().ops(), 0);
}

#[test]
fn direct_io_is_checked_before_submission() {
    let mut r = Rig::new(FileSizes::new(1, 2));
    let f = r.open("/d.dat", rw().with_direct(true));
    let e = r.extent(3);
    let t = |rig: &mut Rig, at: u32, len: u32, off: u64| {
        rig.files
            .write_at(&mut rig.io, &mut rig.x, f, (e, at, len), off)
    };
    assert_eq!(t(&mut r, 0, 1000, 0), Err(FileError::Unaligned));
    assert_eq!(t(&mut r, 512, 4096, 0), Err(FileError::Unaligned));
    assert_eq!(t(&mut r, 0, 4096, 512), Err(FileError::Unaligned));
    let ok = t(&mut r, 0, 8192, 4096).unwrap();
    assert_eq!(r.wait(ok).1, Ok(8192));
    // A read past the end, off a block boundary, is the end of the file.
    let back = r.extent(0);
    assert_eq!(r.read(f, back, EXTENT, 0), Ok(4096 + 8192));
}

#[test]
fn path_operations() {
    let mut r = Rig::new(FileSizes::new(2, 1).with_paths(1));
    let t1 = r.files.mkdir(&mut r.io, "/dir").unwrap();
    // One path slot: the next path operation waits for its event.
    assert_eq!(r.files.unlink(&mut r.io, "/x"), Err(FileError::Full));
    assert_eq!(r.wait(t1), (None, Ok(0)));
    let f = r.open("/dir/a", rw());
    let e = r.extent(7);
    assert_eq!(r.write(f, e, 100, 0), Ok(100));
    r.close(f);
    let t2 = r.files.rename(&mut r.io, "/dir/a", "/dir/b").unwrap();
    assert_eq!(r.wait(t2).1, Ok(0));
    let t3 = r.files.status(&mut r.io, "/dir/b").unwrap();
    assert_eq!(r.wait(t3).1, Ok(100));
    let t4 = r.files.unlink(&mut r.io, "/dir/b").unwrap();
    assert_eq!(r.wait(t4).1, Ok(0));
    let t5 = r.files.status(&mut r.io, "/dir/b").unwrap();
    assert_eq!(r.wait(t5).1, Err(FileError::NotFound));
    let t6 = r
        .files
        .open(&mut r.io, "/dir/none", OpenFlags::NONE.with_read(true))
        .unwrap();
    assert_eq!(r.wait(t6), (None, Err(FileError::NotFound)));
    // A directory is a file: it opens and syncs.
    let d = r.open("/dir", OpenFlags::NONE.with_read(true).with_directory(true));
    assert_eq!(r.sync(d), Ok(0));
}

#[test]
fn a_gathered_write_holds_its_spans_in_order() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/g.dat", rw());
    let (a, b) = (r.extent(b'a'), r.extent(b'b'));
    let spans = [
        Span::Extent {
            extent: a,
            at: 0,
            len: 3,
        },
        Span::Extent {
            extent: b,
            at: 10,
            len: 2,
        },
        Span::Extent {
            extent: a,
            at: 5,
            len: 1,
        },
    ];
    let t = r
        .files
        .write_spans(&mut r.io, &mut r.x, f, &spans, 0)
        .unwrap();
    assert_eq!(r.x.holds(a), Some(3), "held once a span");
    assert_eq!(r.wait(t).1, Ok(6));
    assert_eq!(r.x.holds(a), Some(1));
    let back = r.extent(0);
    assert_eq!(r.read(f, back, 6, 0), Ok(6));
    assert_eq!(&r.io.buf(back.buf())[..6], b"aaabba");
}

#[test]
fn a_sequence_runs_in_order() {
    let mut r = Rig::new(FileSizes::new(2, 3));
    let f = r.open("/s.dat", rw());
    let e = r.extent(9);
    let steps = [
        Step::Write {
            file: f,
            extent: e,
            at: 0,
            len: 4096,
            file_off: 0,
        },
        Step::Sync {
            file: f,
            data_only: true,
        },
        Step::Rename {
            from: "/s.dat",
            to: "/t.dat",
        },
    ];
    let mut tickets = [Ticket::NONE; 3];
    let n = r
        .files
        .sequence(&mut r.io, &mut r.x, &steps, &mut tickets)
        .unwrap();
    assert_eq!(n, 3);
    assert_eq!(r.wait(tickets[0]), (Some(f), Ok(4096)));
    assert_eq!(r.wait(tickets[1]), (Some(f), Ok(0)));
    assert_eq!(r.wait(tickets[2]), (None, Ok(0)));
    // A sequence longer than the plan's is refused whole.
    let long = [Step::Sync {
        file: f,
        data_only: true,
    }; 4];
    let mut t4 = [Ticket::NONE; 4];
    let refused = r.files.sequence(&mut r.io, &mut r.x, &long, &mut t4);
    assert_eq!(refused, Err(FileError::TooLarge));
    // One that does not fit the file's share is refused whole, and takes
    // nothing: the file's three slots are still there afterwards.
    let held = r.files.sync(&mut r.io, f, true).unwrap();
    let three = [Step::Sync {
        file: f,
        data_only: true,
    }; 3];
    let full = r.files.sequence(&mut r.io, &mut r.x, &three, &mut tickets);
    assert_eq!(full, Err(FileError::Full));
    assert_eq!(r.wait(held).1, Ok(0));
    assert_eq!(
        r.files.sequence(&mut r.io, &mut r.x, &three, &mut tickets),
        Ok(3)
    );
    r.settle();
}
