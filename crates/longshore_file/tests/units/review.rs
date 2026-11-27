// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The independent review's cases: gathered and sequenced resumes, the
//! order of slots coming back, poisoned work, a refused chain, the
//! per-file cap with shared slots to spare.

use crate::rig::{disk, pattern, rw, ticket_of, Rig};
use longshore_common::io::{Completion, Io, Limits, OpenFlags, Token};
use longshore_common::sim::{DiskFaults, Faults, PlacedFault, SimIo};
use longshore_file::{FileError, FileSizes, Span, Step, Ticket};

/// The rig's backend replaced by one built with `l`.
fn backend(r: &mut Rig, l: Limits) {
    *r.io = SimIo::new(l, 7, Faults::NONE);
    r.io.setup_disk(disk(4096));
}

/// Drives the backend and hands the set its completions, taking no event.
fn complete_only(r: &mut Rig) {
    for _ in 0..20 {
        r.io.tick(false, &mut r.out).unwrap();
        while let Some(c) = r.out.pop() {
            assert!(r.files.on_completion(&mut r.io, &mut r.x, c));
        }
    }
}

#[test]
fn a_gathered_write_cut_inside_a_span_resumes_with_the_right_bytes() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/g", rw());
    let (a, b) = (r.extent(0), r.extent(0));
    pattern(&mut r, a, 0x01);
    pattern(&mut r, b, 0x80);
    r.io.place_disk_fault(2, PlacedFault::ShortWrite);
    let spans = [
        Span::Extent {
            extent: a,
            at: 100,
            len: 6000,
        },
        Span::Extent {
            extent: b,
            at: 7,
            len: 6288,
        },
    ];
    let t = r
        .files
        .write_spans(&mut r.io, &mut r.x, f, &spans, 0)
        .unwrap();
    assert_eq!(r.wait(t).1, Ok(12_288));
    assert_eq!(
        r.io.disk_fault_counters().short_write(),
        1,
        "the fault fired"
    );
    let back = r.extent(0);
    assert_eq!(r.read(f, back, 12_288, 0), Ok(12_288));
    let mut want = r.io.buf(a.buf())[100..6100].to_vec();
    want.extend_from_slice(&r.io.buf(b.buf())[7..7 + 6288]);
    assert!(r.io.buf(back.buf())[..12_288] == want[..]);
}

#[test]
fn a_direct_gathered_write_at_any_rate_has_the_right_bytes() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/d", rw().with_direct(true));
    let (a, b) = (r.extent(0), r.extent(0));
    pattern(&mut r, a, 0x01);
    pattern(&mut r, b, 0x41);
    r.io.set_disk_faults(DiskFaults::NONE.with_short_write(500_000));
    let spans = [
        Span::Extent {
            extent: a,
            at: 4096,
            len: 8192,
        },
        Span::Extent {
            extent: b,
            at: 0,
            len: 12_288,
        },
    ];
    for k in 0..6u64 {
        let t = r
            .files
            .write_spans(&mut r.io, &mut r.x, f, &spans, k * 20_480)
            .unwrap();
        assert_eq!(r.wait(t).1, Ok(20_480));
    }
    r.io.set_disk_faults(DiskFaults::NONE);
    assert!(r.io.disk_fault_counters().short_write() > 0);
    let back = r.extent(0);
    for k in 0..6u64 {
        assert_eq!(r.read(f, back, 16_384, k * 20_480), Ok(16_384));
        let mut want = r.io.buf(a.buf())[4096..12_288].to_vec();
        want.extend_from_slice(&r.io.buf(b.buf())[..8192]);
        assert!(r.io.buf(back.buf())[..16_384] == want[..], "chunk {k}");
    }
}

#[test]
fn a_sequence_after_a_read_at_the_end_runs_each_step_once_in_order() {
    let mut r = Rig::new(FileSizes::new(1, 3).with_shared(2));
    let f = r.open("/e", rw());
    let e = r.extent(0x11);
    assert_eq!(r.write(f, e, 100, 0), Ok(100));
    let into = r.extent(0);
    let w = r.extent(0x22);
    let steps = [
        Step::Read {
            file: f,
            extent: into,
            at: 0,
            len: 4096,
            file_off: 0,
        },
        Step::Write {
            file: f,
            extent: w,
            at: 0,
            len: 50,
            file_off: 100,
        },
        Step::Status { path: "/e" },
    ];
    let mut t = [Ticket::NONE; 3];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(3));
    assert_eq!(r.wait(t[0]).1, Ok(100));
    assert_eq!(r.wait(t[1]).1, Ok(50));
    assert_eq!(r.wait(t[2]).1, Ok(150));
}

#[test]
fn a_file_slot_opens_again_only_once_its_close_is_taken() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let a = r.open("/a", rw());
    let c = r.files.close(&mut r.io, a).unwrap();
    complete_only(&mut r);
    assert_eq!(r.files.usage().files(), 0, "the slot is Free");
    assert_eq!(r.files.open(&mut r.io, "/b", rw()), Err(FileError::Full));
    let ev = r.files.next_event().unwrap();
    assert_eq!(ticket_of(&ev), c);
    assert!(r.files.open(&mut r.io, "/b", rw()).is_ok());
}

#[test]
fn a_completion_for_nothing_in_flight_is_taken_and_ignored() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let a = r.open("/a", rw());
    let s = r.files.sync(&mut r.io, a, false).unwrap();
    r.io.power_loss();
    r.files.power_lost(&mut r.x);
    // A completion of the sync, as a backend that delivered it late would.
    let base = r.files.config().token_base();
    let late = Completion::new(Token::new(base + u64::from(s.index())), 0);
    assert!(r.files.on_completion(&mut r.io, &mut r.x, late));
    assert!(r.files.next_event().is_none());
    assert_eq!(r.files.usage().ops(), 0);
    // A token past the set's is not its own.
    let tokens = r.files.config().tokens();
    let other = Completion::new(Token::new(base + tokens), 0);
    assert!(!r.files.on_completion(&mut r.io, &mut r.x, other));
}

#[test]
fn power_lost_drops_an_event_not_yet_taken() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let a = r.open("/a", rw());
    let _: Ticket = r.files.sync(&mut r.io, a, false).unwrap();
    complete_only(&mut r);
    r.io.power_loss();
    r.files.power_lost(&mut r.x);
    assert!(r.files.next_event().is_none());
}

#[test]
fn a_poisoned_files_waiting_write_is_never_submitted() {
    let mut r = Rig::new(FileSizes::new(2, 2));
    let small = r.limits.with_ops_max(1);
    backend(&mut r, small);
    let a = r.open("/a", rw());
    let e = r.extent(0x44);
    r.io.place_disk_fault(2, PlacedFault::Fail); // the sync
    let s = r.files.sync(&mut r.io, a, false).unwrap();
    let w = r
        .files
        .write_at(&mut r.io, &mut r.x, a, (e, 0, 4096), 0)
        .unwrap();
    assert_eq!(r.files.usage().waiting(), 1);
    assert_eq!(r.wait(s).1, Err(FileError::SyncFailed { errno: 5 }));
    assert_eq!(r.wait(w).1, Err(FileError::Poisoned));
    let t = r.files.status(&mut r.io, "/a").unwrap();
    assert_eq!(r.wait(t).1, Ok(0), "the write never reached the disk");
    assert_eq!(r.x.holds(e), Some(1));
}

#[test]
fn a_poisoned_files_resumed_write_ends_partial() {
    let mut r = Rig::new(FileSizes::new(1, 2));
    let small = r.limits.with_ops_max(1);
    backend(&mut r, small);
    let a = r.open("/a", rw());
    let e = r.extent(0x45);
    // The write is cut short; the sync behind it fails before it resumes.
    r.io.place_disk_fault(2, PlacedFault::ShortWrite);
    r.io.place_disk_fault(3, PlacedFault::Fail);
    let w = r
        .files
        .write_at(&mut r.io, &mut r.x, a, (e, 0, 12_288), 0)
        .unwrap();
    let s = r.files.sync(&mut r.io, a, false).unwrap();
    assert_eq!(r.wait(s).1, Err(FileError::SyncFailed { errno: 5 }));
    match r.wait(w).1 {
        Err(FileError::Partial { done, kind }) => {
            assert!(done > 0 && done < 12_288);
            assert_eq!(kind, longshore_file::ErrorKind::Poisoned);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_chain_the_backend_refuses_is_a_plain_error_and_poisons_nothing() {
    // A backend with one gather row, which a write of its own holds, and
    // a durable-write file.
    let mut r = Rig::new(FileSizes::new(2, 3));
    let l = r.limits.with_gather(1, r.limits.gather_spans());
    backend(&mut r, l);
    let a = r.open("/a", rw().with_dsync(true));
    let e = r.extent(1);
    let sp = [Span::Extent {
        extent: e,
        at: 0,
        len: 4096,
    }];
    let w1 = r.files.write_spans(&mut r.io, &mut r.x, a, &sp, 0).unwrap();
    let steps = [
        Step::WriteSpans {
            file: a,
            spans: &sp,
            file_off: 4096,
        },
        Step::Sync {
            file: a,
            data_only: true,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(2));
    assert_eq!(r.wait(t[0]).1, Err(FileError::Os { errno: 105 }));
    assert_eq!(r.wait(t[1]).1, Err(FileError::Cancelled));
    assert_eq!(r.wait(w1).1, Ok(4096));
    // The file is not poisoned: it takes writes still.
    assert_eq!(r.write(a, e, 4096, 4096), Ok(4096));
}

#[test]
fn a_chain_refused_behind_its_lead_cancels_the_lead_too() {
    let mut r = Rig::new(FileSizes::new(2, 3));
    let l = r.limits.with_gather(1, r.limits.gather_spans());
    backend(&mut r, l);
    let a = r.open("/a", rw());
    let e = r.extent(1);
    let sp = [Span::Extent {
        extent: e,
        at: 0,
        len: 4096,
    }];
    let w1 = r.files.write_spans(&mut r.io, &mut r.x, a, &sp, 0).unwrap();
    let steps = [
        Step::Sync {
            file: a,
            data_only: true,
        },
        Step::WriteSpans {
            file: a,
            spans: &sp,
            file_off: 4096,
        },
    ];
    let mut t = [Ticket::NONE; 2];
    assert_eq!(r.files.sequence(&mut r.io, &mut r.x, &steps, &mut t), Ok(2));
    assert_eq!(r.wait(t[0]).1, Err(FileError::Cancelled));
    assert_eq!(r.wait(t[1]).1, Err(FileError::Os { errno: 105 }));
    assert_eq!(r.wait(w1).1, Ok(4096));
    assert_eq!(r.sync(a), Ok(0), "nothing poisoned");
}

#[test]
fn a_file_stops_at_its_cap_with_shared_slots_to_spare() {
    let mut r = Rig::new(FileSizes::new(1, 2).with_shared(4));
    let a = r.open("/a", rw());
    let _: Ticket = r.files.sync(&mut r.io, a, true).unwrap();
    let _: Ticket = r.files.sync(&mut r.io, a, true).unwrap();
    assert_eq!(r.files.sync(&mut r.io, a, true), Err(FileError::Full));
    r.settle();
}

#[test]
fn a_directory_opened_without_read_or_write_syncs() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let d = r.open("/", OpenFlags::NONE.with_directory(true));
    assert_eq!(r.sync(d), Ok(0));
}

#[test]
fn a_page_span_must_be_a_page_of_the_receive_area() {
    let mut r = Rig::new(FileSizes::new(1, 1));
    let f = r.open("/p", rw());
    let not_a_page = r.extent(0).buf();
    let spans = [Span::Page {
        page: not_a_page,
        off: 0,
        len: 4096,
    }];
    let w = r.files.write_spans(&mut r.io, &mut r.x, f, &spans, 0);
    assert_eq!(w, Err(FileError::TooLarge));
}
