// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Disk core (Core 1) of `t4_apps_storage::partitioned_service`: a
//! direct-I/O key-value partition served over a mailbox.

use super::super::harness::{write_nul_path, write_pool_bytes};
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore::mailbox::{mailbox, Config, Receiver, Sender, TryRecv};
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};

pub(super) const BLOCK: u32 = 4096;

#[derive(Clone, Copy, Debug)]
pub(super) enum DiskReq {
    Put { key: u32, val: u64, slot: u32 },
    Get { key: u32, slot: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiskResp {
    PutOk(i32),
    GetOk(i32, u64),
}

fn disk_limits() -> Limits {
    Limits::BASE
        .with_sockets_max(2)
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_small_buffers(2)
        .with_small_buffer_size(256)
        .with_aligned_buffers(4)
        .with_aligned_buffer_size(BLOCK)
        .with_block_size(BLOCK)
        .with_files_max(4)
        .with_ops_max(16)
}

fn setup_disk_sim(seed: u64) -> SimIo {
    let mut sim = SimIo::new(disk_limits(), seed, Faults::default());
    sim.setup_disk(
        DiskConfig::BASE
            .with_max_files(8)
            .with_max_dirs(8)
            .with_max_bytes_per_file((BLOCK as usize) * 16)
            .with_max_name_len(64)
            .with_write_log_entries(16)
            .with_write_log_bytes((BLOCK as usize) * 8)
            .with_read_error_sectors(8)
            .with_sector_size(512),
    );
    sim.set_disk_faults(
        DiskFaults::NONE
            .with_short_write(50_000)
            .with_bit_flip(20_000),
    );
    sim
}

/// The disk core: its simulator, limits, completions, and mailbox receiver.
pub(super) struct DiskCore {
    pub(super) disk: SimIo,
    lim: Limits,
    out: Completions,
    io_rx: Receiver<DiskReq, DiskResp>,
}

impl DiskCore {
    /// Builds the faulted disk core for `seed`; returns it and its mailbox sender.
    pub(super) fn new(seed: u64) -> (Self, Sender<DiskReq, DiskResp>) {
        let mut disk = setup_disk_sim(seed);
        let lim = disk_limits();
        let (tx, io_rx) = mailbox::<DiskReq, DiskResp, _>(
            &mut disk,
            Config {
                capacity: 8,
                slots: 8,
                waiters_prealloc: 8,
                wake_token: Token::new(900),
            },
        )
        .expect("mailbox");
        let core = Self {
            disk,
            lim,
            out: Completions::new(16),
            io_rx,
        };
        (core, tx)
    }

    /// Arms the mailbox `AwaitWake` on the disk core.
    pub(super) fn arm(&mut self) {
        self.io_rx.arm(&mut self.disk).unwrap();
    }

    /// One disk tick: wakeups drain and serve every queued request.
    pub(super) fn pump(&mut self) {
        self.disk.tick(false, &mut self.out).unwrap();
        while let Some(c) = self.out.pop() {
            if self.io_rx.on_completion(&mut self.disk, c).unwrap() {
                while let TryRecv::Message(req) = self.io_rx.try_recv() {
                    self.handle(req);
                }
            }
        }
    }

    fn handle(&mut self, req: DiskReq) {
        let path_buf = self.lim.small(0);
        let path_len = write_nul_path(&mut self.disk, path_buf, 0, "/kv_part.dat");
        match req {
            DiskReq::Put { key, val, slot } => {
                let status = self.put(path_buf, path_len, key, val);
                let _: Option<u32> = self.io_rx.answer(slot, DiskResp::PutOk(status));
            }
            DiskReq::Get { key, slot } => {
                let (read_res, val) = self.get(path_buf, path_len, key);
                let _: Option<u32> = self.io_rx.answer(slot, DiskResp::GetOk(read_res, val));
            }
        }
    }

    /// Open, write the key's block, datasync, and close as one chain; returns
    /// 0 or the first failing result.
    fn put(&mut self, path_buf: BufId, path_len: u32, key: u32, val: u64) -> i32 {
        let data_buf = self.lim.aligned(0);
        let mut block = [0u8; BLOCK as usize];
        block[0..4].copy_from_slice(&key.to_le_bytes());
        block[4..12].copy_from_slice(&val.to_le_bytes());
        write_pool_bytes(&mut self.disk, data_buf, 0, &block);
        let wr = Op::FileWrite {
            slot: FileSlot::new(0),
            buf: data_buf,
            off: 0,
            len: BLOCK,
            file_off: u64::from(key) * u64::from(BLOCK),
        };
        let sy = Op::FileSync {
            slot: FileSlot::new(0),
            datasync: true,
        };
        self.disk
            .submit_chain(&[
                (Token::new(100), open_op(path_buf, path_len, true)),
                (Token::new(101), wr),
                (Token::new(102), sy),
                (
                    Token::new(103),
                    Op::FileClose {
                        slot: FileSlot::new(0),
                    },
                ),
            ])
            .unwrap();
        let (write_res, sync_res) = pump_until(&mut self.disk, &mut self.out, 103);
        if write_res == i32::try_from(BLOCK).unwrap() && sync_res == 0 {
            0
        } else if sync_res < 0 {
            sync_res
        } else {
            write_res
        }
    }

    /// Open, read the key's block, and close as one chain; returns the read
    /// result and the stored value (0 on a short read).
    fn get(&mut self, path_buf: BufId, path_len: u32, key: u32) -> (i32, u64) {
        let data_buf = self.lim.aligned(0);
        let rd = Op::FileRead {
            slot: FileSlot::new(0),
            buf: data_buf,
            off: 0,
            len: BLOCK,
            file_off: u64::from(key) * u64::from(BLOCK),
        };
        self.disk
            .submit_chain(&[
                (Token::new(200), open_op(path_buf, path_len, false)),
                (Token::new(201), rd),
                (
                    Token::new(202),
                    Op::FileClose {
                        slot: FileSlot::new(0),
                    },
                ),
            ])
            .unwrap();
        let (read_res, _) = pump_until(&mut self.disk, &mut self.out, 202);
        let val = if read_res >= 12 {
            u64::from_le_bytes(self.disk.buf(data_buf)[4..12].try_into().unwrap())
        } else {
            0
        };
        (read_res, val)
    }
}

/// The direct-I/O open of the partition file (created only when writing).
fn open_op(path_buf: BufId, path_len: u32, write: bool) -> Op {
    Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf,
        path_off: 0,
        path_len,
        flags: OpenFlags::NONE
            .with_read(true)
            .with_write(write)
            .with_create(write)
            .with_direct(true),
    }
}

/// Ticks until `end_tok` completes; returns the results of the chain's
/// `..1` and `..2` ops (the latter only when it is not `end_tok`).
fn pump_until(d: &mut SimIo, o: &mut Completions, end_tok: u64) -> (i32, i32) {
    let (mut r1, mut r2, mut done) = (0i32, 0i32, false);
    while !done {
        d.tick(true, o).unwrap();
        while let Some(c) = o.pop() {
            if c.token().get() % 100 == 1 {
                r1 = c.result();
            } else if c.token().get() % 100 == 2 && c.token().get() != end_tok {
                r2 = c.result();
            }
            if c.token().get() == end_tok {
                done = true;
            }
        }
    }
    (r1, r2)
}
