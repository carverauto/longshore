// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation write-ahead log (`append`, `sync`, `power_loss`, `recover`)
//! verification across all 11 [`DiskFaults`], `corrupt_medium_bit`, delays,
//! multiple seeds, and both 512-byte and 4096-byte sectors.

use longshore_common::io::alloc_guard::Guard;
use longshore_common::io::posix::errno::linux::ECANCELED;
use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore_common::sim::{DiskConfig, Faults, SimIo};

#[global_allocator]
static ALLOC: Guard = Guard;

#[path = "wal/recover.rs"]
mod recover;
#[path = "wal/suite.rs"]
mod suite;

pub(crate) const WAL_MAGIC: u32 = 0x4C57_414C;
pub(crate) const HDR_SIZE: usize = 20;
pub(crate) const MAX_RECS: usize = 12;

const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

pub(crate) fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        let idx = ((crc ^ u32::from(b)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32C_TABLE[idx];
    }
    !crc
}

pub(crate) fn payload_byte(seed: u64, seq: u64, idx: usize) -> u8 {
    let mix = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
        ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (idx as u64);
    (mix ^ (mix >> 29)).rotate_left(11) as u8
}

pub(crate) fn encode_record(buf: &mut [u8], rec_bytes: usize, seed: u64, seq: u64) {
    let payload_len = (rec_bytes - HDR_SIZE) as u32;
    buf[0..4].copy_from_slice(&WAL_MAGIC.to_le_bytes());
    buf[4..12].copy_from_slice(&seq.to_le_bytes());
    buf[12..16].copy_from_slice(&payload_len.to_le_bytes());
    buf[16..20].fill(0);
    for (i, b) in buf[HDR_SIZE..rec_bytes].iter_mut().enumerate() {
        *b = payload_byte(seed, seq, i);
    }
    let mut crc = !0u32;
    for &b in &buf[0..16] {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize];
    }
    for &b in &buf[HDR_SIZE..rec_bytes] {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize];
    }
    buf[16..20].copy_from_slice(&(!crc).to_le_bytes());
}

#[cfg(test)]
pub(crate) fn verify_record(buf: &[u8], rec_bytes: usize, seed: u64, expected_seq: u64) -> bool {
    if buf.len() < rec_bytes {
        return false;
    }
    if u32::from_le_bytes(buf[0..4].try_into().unwrap()) != WAL_MAGIC {
        return false;
    }
    if u64::from_le_bytes(buf[4..12].try_into().unwrap()) != expected_seq {
        return false;
    }
    let payload_len = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
    if payload_len != rec_bytes - HDR_SIZE {
        return false;
    }
    let want_crc = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    let mut crc = !0u32;
    for &b in &buf[0..16] {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize];
    }
    for &b in &buf[HDR_SIZE..rec_bytes] {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize];
    }
    if !crc != want_crc {
        return false;
    }
    buf[HDR_SIZE..rec_bytes]
        .iter()
        .enumerate()
        .all(|(i, &b)| b == payload_byte(seed, expected_seq, i))
}

fn wal_limits() -> Limits {
    Limits {
        buffers: 4,
        buffer_size: 16 * 1024,
        small_buffers: 2,
        small_buffer_size: 128,
        files_max: 2,
        aligned_buffers: 2,
        aligned_buffer_size: 8192,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    }
}

fn wal_disk(sector_size: u32) -> DiskConfig {
    DiskConfig {
        max_files: 4,
        max_dirs: 2,
        max_bytes_per_file: 256 * 1024,
        max_name_len: 64,
        write_log_entries: 32,
        write_log_bytes: 128 * 1024,
        read_error_sectors: 32,
        sector_size,
    }
}

fn open_op(slot: FileSlot, path_buf: BufId, path: &[u8], flags: OpenFlags) -> Op {
    Op::FileOpen {
        slot,
        path_buf,
        path_off: 0,
        path_len: path.len() as u32,
        flags,
    }
}

/// A disk with `/wal.log` open in slot 0 and its directory entry synced.
pub(crate) fn setup_wal(seed: u64, sector_size: u32, delay: u32) -> (SimIo, Completions) {
    let faults = Faults {
        delay,
        ..Default::default()
    };
    let mut sim = SimIo::new(wal_limits(), seed, faults);
    sim.setup_disk(wal_disk(sector_size));
    let mut out = Completions::new(8);
    let p = b"/wal.log\0";
    let d = b"/\0";
    sim.buf(BufId(0))[..p.len()].copy_from_slice(p);
    sim.buf(BufId(3))[..d.len()].copy_from_slice(d);
    let rw_create = OpenFlags {
        read: true,
        write: true,
        create: true,
        ..Default::default()
    };
    let dir = OpenFlags {
        read: true,
        directory: true,
        ..Default::default()
    };
    let mut tok = 1u64;
    let open_log = open_op(FileSlot(0), BufId(0), p, rw_create);
    assert_eq!(drive_one(&mut sim, &mut out, &mut tok, open_log), 0);
    let open_dir = open_op(FileSlot(1), BufId(3), d, dir);
    assert_eq!(drive_one(&mut sim, &mut out, &mut tok, open_dir), 0);
    let sync_dir = Op::FileSync {
        slot: FileSlot(1),
        datasync: false,
    };
    assert_eq!(drive_one(&mut sim, &mut out, &mut tok, sync_dir), 0);
    let close_dir = Op::FileClose { slot: FileSlot(1) };
    assert_eq!(drive(&mut sim, &mut out, &mut tok, close_dir), 0);
    (sim, out)
}

fn drive(sim: &mut SimIo, out: &mut Completions, tok: &mut u64, op: Op) -> i32 {
    drive_one(sim, out, tok, op)
}

#[cfg(test)]
pub(crate) fn drive_one(sim: &mut SimIo, out: &mut Completions, tok: &mut u64, op: Op) -> i32 {
    let t = Token(*tok);
    *tok += 1;
    sim.submit(t, op).unwrap();
    for _ in 0..20_000 {
        sim.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token == t {
                return c.result;
            }
        }
    }
    panic!("timed out on wal op");
}

#[cfg(test)]
pub(crate) fn wal_append(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: &mut u64,
    rec_bytes: usize,
    seed: u64,
    seq: u64,
    sync_failed: &mut bool,
) -> bool {
    encode_record(&mut sim.buf(BufId(1))[..rec_bytes], rec_bytes, seed, seq);
    let mut off = 0usize;
    let file_base = seq * rec_bytes as u64;
    while off < rec_bytes {
        let wt = Token(*tok);
        let st = Token(*tok + 1);
        *tok += 2;
        sim.submit_chain(&[
            (
                wt,
                Op::FileWrite {
                    slot: FileSlot(0),
                    buf: BufId(1),
                    off: off as u32,
                    len: (rec_bytes - off) as u32,
                    file_off: file_base + off as u64,
                },
            ),
            (
                st,
                Op::FileSync {
                    slot: FileSlot(0),
                    datasync: false,
                },
            ),
        ])
        .unwrap();
        let mut wr = None;
        let mut sr = None;
        while wr.is_none() || sr.is_none() {
            sim.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == wt {
                    wr = Some(c.result);
                } else if c.token == st {
                    sr = Some(c.result);
                }
            }
        }
        let w = wr.unwrap();
        let s = sr.unwrap();
        if w < 0 {
            assert_eq!(s, -ECANCELED);
            return false;
        }
        let written = usize::try_from(w).unwrap();
        if written < rec_bytes - off {
            assert_eq!(s, -ECANCELED);
            off += written;
            continue;
        }
        if s < 0 {
            *sync_failed = true;
            return false;
        }
        return true;
    }
    true
}
