// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for Feature 12 (`t12_file_chains`).

#[cfg(unix)]
use super::super::harness::posix_backend;
use super::super::harness::{drive_until, submit_and_wait_one, write_nul_path, TempDir};
#[cfg(target_os = "linux")]
use longshore::io::uring::UringIo;
use longshore::io::{BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};

pub const LINUX_EINVAL: i32 = 22;

pub fn chain_limits(ops_max: u32) -> Limits {
    Limits::BASE
        .with_sockets_max(4)
        .with_buffers(2)
        .with_buffer_size(8192)
        .with_small_buffers(2)
        .with_small_buffer_size(512)
        .with_files_max(4)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_ops_max(ops_max)
}

#[cfg(feature = "sim")]
pub fn make_sim(limits: Limits, seed: u64) -> SimIo {
    let mut sim = SimIo::new(limits, seed, Faults::default());
    sim.setup_disk(
        longshore::sim::DiskConfig::BASE
            .with_max_files(32)
            .with_max_dirs(16)
            .with_max_bytes_per_file(1024 * 1024)
            .with_max_name_len(256)
            .with_write_log_entries(64)
            .with_write_log_bytes(256 * 1024)
            .with_read_error_sectors(16)
            .with_sector_size(512),
    );
    sim
}

pub fn run_on_backends(limits: Limits, mut check: impl FnMut(&mut dyn Io, &str)) {
    #[cfg(feature = "sim")]
    {
        let mut sim = make_sim(limits, 31);
        check(&mut sim, "");
    }
    #[cfg(unix)]
    {
        let dir = TempDir::new("e2e_f12_posix").expect("temp dir");
        let mut posix = posix_backend(limits).expect("posix");
        let prefix = format!("{}/", dir.path().display());
        check(&mut posix, &prefix);
    }
    #[cfg(target_os = "linux")]
    {
        let dir = TempDir::new("e2e_f12_uring").expect("temp dir");
        let mut uring = UringIo::new(limits).expect("uring");
        let prefix = format!("{}/", dir.path().display());
        check(&mut uring, &prefix);
    }
}

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

pub fn open_slot(
    io: &mut dyn Io,
    out: &mut Completions,
    tok: u64,
    slot: FileSlot,
    path_buf: BufId,
    path: &str,
    flags: OpenFlags,
) -> i32 {
    let mut d = DynIo(io);
    let path_len = write_nul_path(&mut d, path_buf, 0, path);
    submit_and_wait_one(
        &mut d,
        out,
        Token::new(tok),
        Op::FileOpen {
            slot,
            path_buf,
            path_off: 0,
            path_len,
            flags,
        },
        10_000,
    )
    .expect("open")
    .result()
}

pub fn close_slot(io: &mut dyn Io, out: &mut Completions, tok: u64, slot: FileSlot) -> i32 {
    submit_and_wait_one(
        &mut DynIo(io),
        out,
        Token::new(tok),
        Op::FileClose { slot },
        10_000,
    )
    .expect("close")
    .result()
}

pub fn submit_chain_and_collect(
    io: &mut dyn Io,
    out: &mut Completions,
    ops: &[(Token, Op)],
) -> Vec<Completion> {
    let mut d = DynIo(io);
    d.submit_chain(ops).expect("submit_chain");
    let expected: Vec<Token> = ops.iter().map(|(t, _)| *t).collect();
    drive_until(&mut d, out, 10_000, |done| {
        expected
            .iter()
            .all(|t| done.iter().any(|c| c.token() == *t))
    })
    .expect("chain completion")
}
