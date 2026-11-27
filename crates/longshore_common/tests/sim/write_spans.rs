// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A write gathered from a list of spans of the pool (`Op::FileWriteSpans`)
//! on the simulated disk: block-aligned buffers and pages of the receive
//! area in one write, read at the write's completion, and what is
//! refused. Under `alloc_guard`.

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::{
    BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, EINVAL,
};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[global_allocator]
static ALLOC: Guard = Guard;

const BLOCK: u32 = 4096;
const LIMITS: Limits = Limits::BASE
    .with_buffers(2)
    .with_buffer_size(1024)
    .with_aligned_buffers(3)
    .with_aligned_buffer_size(4 * BLOCK)
    .with_area_pages(4)
    .with_files_max(1)
    .with_gather(2, 8)
    .with_ops_max(8);
const FILE: FileSlot = FileSlot::new(0);
/// Buffer 0 holds the path, buffer 1 the list of spans.
const LIST: BufId = BufId::new(1);

#[path = "write_spans/faults.rs"]
mod faults;

/// A simulator with a file open in slot 0, for direct I/O or not.
#[cfg(test)]
fn opened(direct: bool) -> (SimIo, Completions) {
    opened_with(9, OpenFlags::NONE.with_direct(direct))
}

/// A simulator of `seed` with a file open in slot 0, for reading and
/// writing with `flags` besides.
#[cfg(test)]
fn opened_with(seed: u64, flags: OpenFlags) -> (SimIo, Completions) {
    let mut io = SimIo::new(LIMITS, seed, Faults::NONE);
    io.setup_disk(
        DiskConfig::BASE
            .with_max_files(4)
            .with_max_dirs(4)
            .with_max_bytes_per_file(64 * 1024)
            .with_max_name_len(64),
    );
    let mut out = Completions::new(8);
    let path = b"log\0";
    io.buf(BufId::new(0))[..path.len()].copy_from_slice(path);
    let flags = flags.with_read(true).with_write(true).with_create(true);
    let open = Op::FileOpen {
        slot: FILE,
        path_buf: BufId::new(0),
        path_off: 0,
        path_len: path.len() as u32,
        flags,
    };
    assert_eq!(run(&mut io, &mut out, open), 0);
    (io, out)
}

/// Submit `op` and turn until it completes: its result.
#[cfg(test)]
fn run(io: &mut SimIo, out: &mut Completions, op: Op) -> i32 {
    io.submit(Token::new(7), op).unwrap();
    loop {
        io.tick(true, out).unwrap();
        if let Some(c) = out.pop() {
            return c.result();
        }
    }
}

/// Write the list of `spans` into the list buffer; the gathered write of
/// them at `file_off`, said to be `len` bytes.
#[cfg(test)]
fn gather(io: &mut SimIo, spans: &[(BufId, u32, u32)], len: u32, file_off: u64) -> Op {
    let list = io.buf(LIST);
    for (i, (buf, off, part)) in spans.iter().enumerate() {
        for (k, word) in [buf.get(), *off, *part].iter().enumerate() {
            list[i * 12 + 4 * k..i * 12 + 4 * k + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    Op::FileWriteSpans {
        slot: FILE,
        list: LIST,
        list_off: 0,
        count: spans.len() as u32,
        len,
        file_off,
    }
}

/// Fill `len` bytes of `buf` from `off` with `byte`.
#[cfg(test)]
fn fill(io: &mut SimIo, buf: BufId, off: u32, len: u32, byte: u8) {
    io.buf(buf)[off as usize..(off + len) as usize].fill(byte);
}

/// The first `len` bytes of the file, read back into the third aligned
/// buffer.
#[cfg(test)]
fn read_back(io: &mut SimIo, out: &mut Completions, len: u32) -> BufId {
    let into = LIMITS.aligned(2);
    let read = Op::FileRead {
        slot: FILE,
        buf: into,
        off: 0,
        len,
        file_off: 0,
    };
    assert_eq!(run(io, out, read), len.cast_signed());
    into
}

/// Blocks of an aligned buffer, a page of the receive area and blocks of
/// another buffer go to a file opened for direct I/O as one write, in
/// the list's order.
#[test]
fn aligned_buffers_and_a_page_go_out_as_one_direct_write() {
    let (mut io, mut out) = opened(true);
    let (a, page, b) = (LIMITS.aligned(0), LIMITS.page(2), LIMITS.aligned(1));
    fill(&mut io, a, 0, BLOCK, 0xA1);
    fill(&mut io, page, 0, BLOCK, 0xB2);
    fill(&mut io, b, BLOCK, BLOCK, 0xC3);
    let spans = [(a, 0, BLOCK), (page, 0, BLOCK), (b, BLOCK, BLOCK)];
    sealed(|| {
        let write = gather(&mut io, &spans, 3 * BLOCK, 0);
        assert_eq!(run(&mut io, &mut out, write), (3 * BLOCK).cast_signed());
        let got = read_back(&mut io, &mut out, 3 * BLOCK);
        let bytes = io.buf(got);
        for (i, byte) in [0xA1u8, 0xB2, 0xC3].iter().enumerate() {
            let block = &bytes[i * BLOCK as usize..(i + 1) * BLOCK as usize];
            assert!(block.iter().all(|x| x == byte), "block {i}");
        }
    });
}

/// The spans' memory is read when the write completes: what is written
/// into a span between the submit and the completion is what the file
/// holds. A caller keeps its memory as it is until then.
#[test]
fn the_spans_are_read_at_the_completion() {
    let (mut io, mut out) = opened(true);
    let page = LIMITS.page(0);
    fill(&mut io, page, 0, BLOCK, 0x11);
    let write = gather(&mut io, &[(page, 0, BLOCK)], BLOCK, 0);
    io.submit(Token::new(7), write).unwrap();
    fill(&mut io, page, 0, BLOCK, 0x22);
    let mut done = None;
    while done.is_none() {
        io.tick(true, &mut out).unwrap();
        done = out.pop().map(|c| c.result());
    }
    assert_eq!(done, Some(BLOCK.cast_signed()));
    let got = read_back(&mut io, &mut out, BLOCK);
    assert!(io.buf(got)[..BLOCK as usize].iter().all(|x| *x == 0x22));
}

/// What the gathered write of `spans`, said to be `len` bytes, ends with.
#[cfg(test)]
fn refused(io: &mut SimIo, out: &mut Completions, spans: &[(BufId, u32, u32)], len: u32) -> i32 {
    let write = gather(io, spans, len, 0);
    run(io, out, write)
}

/// A list that is empty, lengths that do not add up to what is said, a
/// span outside its buffer, and for direct I/O a span that is not whole
/// blocks of a block-aligned buffer or a page: refused, nothing written.
#[test]
fn a_list_that_cannot_be_written_is_refused() {
    let (mut io, mut out) = opened(true);
    let (a, small) = (LIMITS.aligned(0), BufId::new(0));
    assert_eq!(refused(&mut io, &mut out, &[], 0), -EINVAL);
    assert_eq!(
        refused(&mut io, &mut out, &[(a, 0, BLOCK)], 2 * BLOCK),
        -EINVAL
    );
    assert_eq!(
        refused(&mut io, &mut out, &[(a, 3 * BLOCK, 2 * BLOCK)], 2 * BLOCK),
        -EINVAL
    );
    assert_eq!(refused(&mut io, &mut out, &[(a, 0, 100)], 100), -EINVAL);
    assert_eq!(refused(&mut io, &mut out, &[(a, 8, BLOCK)], BLOCK), -EINVAL);
    assert_eq!(refused(&mut io, &mut out, &[(small, 0, 512)], 512), -EINVAL);
    let off_block = Op::FileWriteSpans {
        slot: FILE,
        list: LIST,
        list_off: 0,
        count: 1,
        len: BLOCK,
        file_off: 100,
    };
    let _list: Op = gather(&mut io, &[(a, 0, BLOCK)], BLOCK, 0);
    assert_eq!(run(&mut io, &mut out, off_block), -EINVAL);
}

/// Without direct I/O any spans of any buffers are gathered.
#[test]
fn a_file_without_direct_io_takes_any_spans() {
    let (mut io, mut out) = opened(false);
    let (small, page) = (BufId::new(0), LIMITS.page(1));
    fill(&mut io, small, 100, 7, 0x5A);
    fill(&mut io, page, 9, 3, 0x6B);
    let write = gather(&mut io, &[(small, 100, 7), (page, 9, 3)], 10, 0);
    assert_eq!(run(&mut io, &mut out, write), 10);
    let got = read_back(&mut io, &mut out, 10);
    assert_eq!(
        &io.buf(got)[..10],
        &[0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x6B, 0x6B, 0x6B]
    );
}
