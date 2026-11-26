// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A frame kept in its pages written to a file opened for direct I/O, as
//! one gathered write of its spans (`Op::FileWriteSpans`), under the
//! hold of the write alone: what is read back is the frame, its last
//! block padded with zeros. The simulator reads the spans when the write
//! completes, so a page given back before that would show.

use super::paged::{exchange, none_out, paired, settle, world};
use super::*;
use longshore_common::io::{BufId, FileSlot, OpenFlags};
use longshore_common::sim::DiskConfig;
use longshore_tcp::Span;

const FILE: FileSlot = FileSlot::new(0);
const MINE: Token = Token::new(1 << 41);

/// A world whose backend has a disk with room for one file.
fn with_disk(seed: u64) -> World {
    let w = world(seed, Faults::NONE);
    // One file, and one gathered write of a frame's pages in flight.
    let limits = w.limits.with_files_max(1).with_gather(1, 64);
    let mut io = SimIo::new(limits, seed, Faults::NONE);
    io.setup_disk(
        DiskConfig::BASE
            .with_max_files(2)
            .with_max_dirs(2)
            .with_max_bytes_per_file(64 * 1024)
            .with_max_name_len(16),
    );
    World { io, limits, ..w }
}

/// Submit `op`, turning both sets meanwhile, until it completes.
fn run(w: &mut World, op: Op) -> i32 {
    w.io.submit(MINE, op).unwrap();
    for _ in 0..10_000 {
        let now = w.io.now_ns();
        w.client.poll(&mut w.io, now);
        w.server.poll(&mut w.io, now);
        if !w.ticking {
            w.io.submit(TICK, Op::Timeout { nanos: MS / 4 }).unwrap();
            w.ticking = true;
        }
        w.io.tick(true, &mut w.out).unwrap();
        let mut result = None;
        while let Some(c) = w.out.pop() {
            if c.token() == MINE {
                result = Some(c.result());
            } else if c.token() == TICK {
                w.ticking = false;
            } else {
                let taken =
                    w.client.on_completion(&mut w.io, c) || w.server.on_completion(&mut w.io, c);
                assert!(taken, "{c:?}");
            }
        }
        if let Some(result) = result {
            return result;
        }
    }
    panic!("the file operation never completed");
}

/// Open the file for direct I/O, its path in `scratch`.
fn open(w: &mut World, scratch: BufId) {
    let path = b"wal\0";
    w.io.buf(scratch)[..path.len()].copy_from_slice(path);
    let flags = OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true)
        .with_direct(true);
    let op = Op::FileOpen {
        slot: FILE,
        path_buf: scratch,
        path_off: 0,
        path_len: path.len() as u32,
        flags,
    };
    assert_eq!(run(w, op), 0);
}

/// A frame of `len` bytes that arrives in its pages, and its bytes.
fn kept(w: &mut World, pair: (Conn, Conn), len: usize) -> (Frame, Vec<u8>) {
    for i in 0..200u8 {
        let frame = prefixed(len, i);
        let f = exchange(w, pair, core::slice::from_ref(&frame), |_| true)[0];
        if f.run().is_some() {
            return (f, frame);
        }
        w.server.release(f);
    }
    panic!("no frame arrived in its pages");
}

#[test]
fn a_kept_frame_goes_to_disk_as_one_write_of_whole_blocks() {
    for seed in 0..10 {
        let mut w = with_disk(seed);
        let (list, back) = (
            w.server.extents().take().unwrap(),
            w.server.extents().take().unwrap(),
        );
        open(&mut w, list.buf());
        let pair = paired(&mut w);
        // A frame with a longer last block first: its tail block is the
        // next one's, and must be zero beyond that one's bytes again.
        let (longer, _) = kept(&mut w, pair, 8000);
        w.server.release(longer);
        settle(&mut w);
        let (f, sent) = kept(&mut w, pair, 6000);
        let run_of = f.run().unwrap();
        // Whole pages, and the tail block taken whole: it is zero beyond.
        let spans: Vec<Span> = w.server.pages(&f).unwrap().collect();
        let mut len = 0;
        for (i, span) in spans.iter().enumerate() {
            let whole = Limits::PAGE.max(span.len());
            let words = [span.buf().get(), span.off(), whole];
            for (k, word) in words.iter().enumerate() {
                let at = i * 12 + 4 * k;
                w.io.buf(list.buf())[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
            len += whole;
        }
        // The write's hold alone keeps the pages from here.
        w.server.runs().hold(run_of).unwrap();
        w.server.release(f);
        let write = Op::FileWriteSpans {
            slot: FILE,
            list: list.buf(),
            list_off: 0,
            count: spans.len() as u32,
            len,
            file_off: 0,
        };
        assert_eq!(run(&mut w, write), len.cast_signed());
        assert_eq!(w.server.runs().release(run_of), Ok(true));
        let read = Op::FileRead {
            slot: FILE,
            buf: back.buf(),
            off: 0,
            len,
            file_off: 0,
        };
        assert_eq!(run(&mut w, read), len.cast_signed());
        let on_disk = &w.io.buf(back.buf())[..len as usize];
        assert_eq!(&on_disk[..sent.len()], &sent[..]);
        assert!(on_disk[sent.len()..].iter().all(|b| *b == 0));
        for ext in [list, back] {
            let _freed: bool = w.server.extents().release(ext).unwrap();
        }
        settle(&mut w);
        none_out(&mut w);
    }
}
