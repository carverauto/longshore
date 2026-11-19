// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for Features 9–10 (`t12_pool_fileslots`).

#[cfg(unix)]
use super::super::harness::posix_backend;
use super::super::harness::{submit_and_wait_one, write_nul_path, TempDir};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

#[allow(dead_code)]
pub const LINUX_EEXIST: i32 = 17;
pub const LINUX_EBUSY: i32 = 16;
pub const LINUX_EINVAL: i32 = 22;

pub fn file_limits(
    files_max: u32,
    aligned_buffers: u32,
    aligned_buffer_size: u32,
    block_size: u32,
) -> Limits {
    Limits {
        sockets_max: 4,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 512,
        files_max,
        aligned_buffers,
        aligned_buffer_size,
        block_size,
        ops_max: 16,
        ..Limits::BASE
    }
}

#[cfg(feature = "sim")]
pub fn make_sim_with_disk(limits: Limits, seed: u64) -> SimIo {
    let mut sim = SimIo::new(limits, seed, Faults::default());
    sim.setup_disk(longshore::sim::DiskConfig {
        max_files: 32,
        max_dirs: 16,
        max_bytes_per_file: 1024 * 1024,
        max_name_len: 256,
        write_log_entries: 64,
        write_log_bytes: 256 * 1024,
        read_error_sectors: 16,
        sector_size: 512,
    });
    sim
}

pub fn run_on_backends(limits: Limits, mut check: impl FnMut(&mut dyn Io, &str)) {
    #[cfg(feature = "sim")]
    {
        let mut sim = make_sim_with_disk(limits, 11);
        check(&mut sim, "");
    }
    #[cfg(unix)]
    {
        let dir = TempDir::new("e2e_f09_10").expect("temp dir");
        let mut posix = posix_backend(limits).expect("posix backend");
        let prefix = format!("{}/", dir.path().display());
        check(&mut posix, &prefix);
    }
    #[cfg(target_os = "linux")]
    {
        let dir = TempDir::new("e2e_f09_10_uring").expect("temp dir");
        let mut uring = UringIo::new(limits).expect("uring backend");
        let prefix = format!("{}/", dir.path().display());
        check(&mut uring, &prefix);
    }
}

pub fn open_slot(
    io: &mut dyn Io,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    path_buf: BufId,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    let path_len = write_nul_path(&mut DynIo(io), path_buf, 0, path);
    submit_and_wait_one(
        &mut DynIo(io),
        out,
        Token(tok),
        Op::FileOpen {
            slot,
            path_buf,
            path_off: 0,
            path_len,
            flags,
        },
        10_000,
    )
    .expect("open tick")
    .result
}

pub fn close_slot(io: &mut dyn Io, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    submit_and_wait_one(
        &mut DynIo(io),
        out,
        Token(tok),
        Op::FileClose { slot },
        10_000,
    )
    .expect("close tick")
    .result
}

/// Thin wrapper so `&mut dyn Io` can pass to generic `IO: Io` harness helpers.
pub struct DynIo<'a>(pub &'a mut dyn Io);

impl Io for DynIo<'_> {
    fn now_ns(&self) -> u64 {
        self.0.now_ns()
    }
    fn unix_secs(&self) -> u64 {
        self.0.unix_secs()
    }
    fn buf(&mut self, id: BufId) -> &mut [u8] {
        self.0.buf(id)
    }
    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.0.buf_pair(a, b)
    }
    fn register_file(&mut self, path: &str) -> std::io::Result<longshore::io::FileId> {
        self.0.register_file(path)
    }
    fn listen(
        &mut self,
        sock: longshore::io::Sock,
        addr: std::net::SocketAddr,
    ) -> std::io::Result<std::net::SocketAddr> {
        self.0.listen(sock, addr)
    }
    fn listen_unix(&mut self, sock: longshore::io::Sock, path: &str) -> std::io::Result<()> {
        self.0.listen_unix(sock, path)
    }
    fn no_delay(&mut self, on: bool) {
        self.0.no_delay(on);
    }
    fn keep_alive(&mut self, on: bool) {
        self.0.keep_alive(on);
    }
    fn measure_segment_sizes(&mut self, on: bool) {
        self.0.measure_segment_sizes(on);
    }
    fn waker(&mut self) -> Option<std::sync::Arc<dyn longshore::io::Wake>> {
        self.0.waker()
    }
    fn submit(&mut self, token: Token, op: Op) -> Result<(), longshore::io::Busy> {
        self.0.submit(token, op)
    }
    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), longshore::io::Busy> {
        self.0.submit_chain(ops)
    }
    fn tick(&mut self, wait: bool, out: &mut Completions) -> std::io::Result<()> {
        self.0.tick(wait, out)
    }
    fn give_back(&mut self, id: BufId) {
        self.0.give_back(id);
    }
}
