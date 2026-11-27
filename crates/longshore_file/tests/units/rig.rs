// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A set on a simulator with a disk, and a loop that drives it.

use longshore_common::io::extents::{Extent, Extents};
use longshore_common::io::{Completions, Io, Limits, OpenFlags};
use longshore_common::sim::{DiskConfig, Faults, SimIo};
use longshore_file::{Event, File, FileConfig, FileError, FileSizes, Files, Ticket};

/// Bytes of each block-aligned buffer.
pub const EXTENT: u32 = 16 << 10;

/// What every rig's backend has before the set is planned onto it.
pub fn base() -> Limits {
    Limits::BASE
        .with_small_buffer_size(4096)
        .with_aligned_buffers(8)
        .with_aligned_buffer_size(EXTENT)
        .with_block_size(4096)
}

pub struct Rig {
    pub io: SimIo,
    pub files: Files,
    pub x: Extents,
    pub limits: Limits,
    pub out: Completions,
    /// Events taken, oldest first, until a test claims them.
    pub events: Vec<Event>,
}

impl Rig {
    pub fn new(sizes: FileSizes) -> Rig {
        let (limits, config) = FileConfig::plan(base(), sizes).unwrap();
        let mut io = SimIo::new(limits, 7, Faults::NONE);
        io.setup_disk(
            DiskConfig::BASE
                .with_max_files(16)
                .with_max_dirs(8)
                .with_max_bytes_per_file(1 << 20)
                .with_max_name_len(256)
                .with_sector_size(4096),
        );
        Rig {
            io,
            files: Files::new(config, &limits),
            x: Extents::new(limits.aligned(0), limits.aligned_buffers()),
            limits,
            out: Completions::new(64),
            events: Vec::new(),
        }
    }

    /// One turn: tick, hand the set its completions, poll, take events.
    pub fn turn(&mut self) {
        self.io.tick(false, &mut self.out).unwrap();
        while let Some(c) = self.out.pop() {
            assert!(self.files.on_completion(&mut self.io, &mut self.x, c));
        }
        self.files.poll(&mut self.io, &mut self.x);
        while let Some(e) = self.files.next_event() {
            self.events.push(e);
        }
    }

    /// Turns until `t` has reported; its file and result.
    pub fn wait(&mut self, t: Ticket) -> (Option<File>, Result<u64, FileError>) {
        for _ in 0..10_000 {
            if let Some(k) = self.events.iter().position(|e| ticket_of(e) == t) {
                let Event::Done { file, result, .. } = self.events.remove(k);
                return (file, result);
            }
            self.turn();
        }
        panic!("ticket {t:?} never reported");
    }

    /// Turns until nothing is left in flight.
    pub fn settle(&mut self) {
        for _ in 0..10_000 {
            self.turn();
            let u = self.files.usage();
            if u.ops() == 0 && u.waiting() == 0 {
                return;
            }
        }
        panic!("the set never settled");
    }

    pub fn open(&mut self, path: &str, flags: OpenFlags) -> File {
        let t = self.files.open(&mut self.io, path, flags).unwrap();
        let (file, result) = self.wait(t);
        assert_eq!(result, Ok(0), "open {path}");
        file.unwrap()
    }

    pub fn close(&mut self, file: File) {
        let t = self.files.close(&mut self.io, file).unwrap();
        assert_eq!(self.wait(t).1, Ok(0));
    }

    /// An extent filled with `byte`.
    pub fn extent(&mut self, byte: u8) -> Extent {
        let e = self.x.take().unwrap();
        self.io.buf(e.buf()).fill(byte);
        e
    }

    pub fn write(&mut self, file: File, e: Extent, len: u32, off: u64) -> Result<u64, FileError> {
        let t = self
            .files
            .write_at(&mut self.io, &mut self.x, file, (e, 0, len), off)
            .unwrap();
        self.wait(t).1
    }

    pub fn read(&mut self, file: File, e: Extent, len: u32, off: u64) -> Result<u64, FileError> {
        let t = self
            .files
            .read_at(&mut self.io, &mut self.x, file, (e, 0, len), off)
            .unwrap();
        self.wait(t).1
    }

    pub fn sync(&mut self, file: File) -> Result<u64, FileError> {
        let t = self.files.sync(&mut self.io, file, false).unwrap();
        self.wait(t).1
    }
}

pub fn ticket_of(e: &Event) -> Ticket {
    let Event::Done { ticket, .. } = *e;
    ticket
}

pub fn rw() -> OpenFlags {
    OpenFlags::NONE
        .with_read(true)
        .with_write(true)
        .with_create(true)
}
