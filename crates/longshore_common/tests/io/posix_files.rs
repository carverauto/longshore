// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared file and chain contract tests plus zero-allocation check on [`PosixIo`].

#![cfg(unix)]

use longshore_common::io::alloc_guard::{sealed, Guard};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{
    contract_chains, contract_files, BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token,
};
use std::path::PathBuf;

#[global_allocator]
static ALLOC: Guard = Guard;

struct TempDir(PathBuf);

#[cfg(test)]
impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "longshore_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop must not panic over a leftover temporary file.
        let _: Option<()> = std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn posix_file_and_chain_contracts_with_zero_alloc() {
    let dir_f = TempDir::new("posix_files");
    let prefix_f = format!("{}/", dir_f.0.display());
    contract_files::run_all(&mut |l| PosixIo::new(l).unwrap(), &prefix_f);

    let dir_c = TempDir::new("posix_chains");
    let prefix_c = format!("{}/", dir_c.0.display());
    contract_chains::run_all(&mut |l| PosixIo::new(l).unwrap(), &prefix_c);

    let dir_s = TempDir::new("posix_sealed");
    let path = format!("{}/sealed.dat\0", dir_s.0.display());
    let limits = Limits::BASE
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_files_max(2)
        .with_ops_max(8);
    let mut io = PosixIo::new(limits).unwrap();
    let mut out = Completions::new(8);
    io.buf(BufId::new(0))[..path.len()].copy_from_slice(path.as_bytes());
    io.buf(BufId::new(1))[..8].copy_from_slice(b"posixwal");

    sealed(|| {
        io.submit_chain(&[
            (
                Token::new(1),
                Op::FileOpen {
                    slot: FileSlot::new(0),
                    path_buf: BufId::new(0),
                    path_off: 0,
                    path_len: path.len() as u32,
                    flags: OpenFlags::NONE
                        .with_read(true)
                        .with_write(true)
                        .with_create(true),
                },
            ),
            (
                Token::new(2),
                Op::FileWrite {
                    slot: FileSlot::new(0),
                    buf: BufId::new(1),
                    off: 0,
                    len: 8,
                    file_off: 0,
                },
            ),
            (
                Token::new(3),
                Op::FileSync {
                    slot: FileSlot::new(0),
                    datasync: true,
                },
            ),
            (
                Token::new(4),
                Op::FileClose {
                    slot: FileSlot::new(0),
                },
            ),
        ])
        .unwrap();
        let mut done = 0;
        while done < 4 {
            io.tick(true, &mut out).unwrap();
            while let Some(c) = out.pop() {
                assert!(c.result() >= 0);
                done += 1;
            }
        }
    });
}
