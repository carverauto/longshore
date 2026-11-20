// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `alloc_guard`-sealed suite for `SimDisk` WAL append, `submit_chain`,
//! `DiskFaults`, `power_loss`, and zero-allocation CRC-64/NVME recovery.

use longshore::checksum::Crc64Nvme;
use longshore::io::alloc_guard::sealed;
use longshore::io::{Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore::sim::{DiskConfig, DiskFaults, Faults, SimIo};

const WAL_MAGIC: u32 = 0x5741_4C31;
const HEADER_LEN: usize = 24;

fn wal_crc(seq: u64, payload: &[u8]) -> u64 {
    let mut c = Crc64Nvme::new();
    c.update(&seq.to_le_bytes());
    c.update(&(payload.len() as u32).to_le_bytes());
    c.update(payload);
    c.finish()
}

fn drive_one_sync<IO: Io>(io: &mut IO, out: &mut Completions, tok: Token, op: Op) -> i32 {
    if io.submit(tok, op).is_err() {
        return -1;
    }
    for _ in 0..2048 {
        if io.tick(false, out).is_err() {
            return -1;
        }
        while let Some(c) = out.pop() {
            if c.token == tok {
                return c.result;
            }
        }
    }
    -1
}

pub fn run_simdisk_wal_recovery_sealed() {
    let limits = Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 256,
        files_max: 2,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut sim = SimIo::new(limits, 9001, Faults::default());
    sim.setup_disk(DiskConfig {
        max_files: 4,
        max_dirs: 4,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 64,
        write_log_entries: 16,
        write_log_bytes: 32 * 1024,
        read_error_sectors: 4,
        sector_size: 512,
    });
    sim.set_disk_faults(DiskFaults {
        short_write: 50_000,
        bit_flip: 50_000,
        ..Default::default()
    });
    let mut out = Completions::new(8);
    let path_buf = limits.small(0);
    let wbuf = limits.aligned(0);
    let rbuf = limits.aligned(1);
    let seg_path = b"/wal_0001.log\0";
    sim.buf(path_buf)[..seg_path.len()].copy_from_slice(seg_path);

    let (committed, recovered) = sealed(|| {
        let open_flags = OpenFlags {
            read: true,
            write: true,
            create: true,
            direct: true,
            ..Default::default()
        };
        if drive_one_sync(
            &mut sim,
            &mut out,
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf,
                path_off: 0,
                path_len: seg_path.len() as u32,
                flags: open_flags,
            },
        ) != 0
        {
            return (0u32, 0u32);
        }

        let mut committed_cnt = 0u32;
        let mut cursor = 0u64;
        let payload = [0xA7u8; 48];
        for seq in 1..=6u64 {
            let sum = wal_crc(seq, &payload);
            let b = sim.buf(wbuf);
            b[..4096].fill(0);
            b[0..4].copy_from_slice(&WAL_MAGIC.to_le_bytes());
            b[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            b[8..16].copy_from_slice(&seq.to_le_bytes());
            b[16..24].copy_from_slice(&sum.to_le_bytes());
            b[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(&payload);

            let tw = Token(10 + seq * 2);
            let ts = Token(11 + seq * 2);
            if sim
                .submit_chain(&[
                    (
                        tw,
                        Op::FileWrite {
                            slot: FileSlot(0),
                            buf: wbuf,
                            off: 0,
                            len: 4096,
                            file_off: cursor,
                        },
                    ),
                    (
                        ts,
                        Op::FileSync {
                            slot: FileSlot(0),
                            datasync: true,
                        },
                    ),
                ])
                .is_err()
            {
                break;
            }
            let (mut w_res, mut s_res) = (-1, -1);
            let (mut gw, mut gs) = (false, false);
            for _ in 0..64 {
                if sim.tick(false, &mut out).is_err() {
                    break;
                }
                while let Some(c) = out.pop() {
                    if c.token == tw {
                        w_res = c.result;
                        gw = true;
                    } else if c.token == ts {
                        s_res = c.result;
                        gs = true;
                    }
                }
                if gw && gs {
                    break;
                }
            }
            if w_res == 4096 && s_res == 0 {
                committed_cnt += 1;
                cursor += 4096;
            } else {
                break;
            }
        }

        sim.power_loss();
        sim.set_disk_faults(DiskFaults::default());
        sim.buf(path_buf)[..seg_path.len()].copy_from_slice(seg_path);
        if drive_one_sync(
            &mut sim,
            &mut out,
            Token(100),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf,
                path_off: 0,
                path_len: seg_path.len() as u32,
                flags: OpenFlags {
                    read: true,
                    direct: true,
                    ..Default::default()
                },
            },
        ) != 0
        {
            return (committed_cnt, 0);
        }

        let mut rec_cnt = 0u32;
        let mut off = 0u64;
        for idx in 0..8u64 {
            let n = drive_one_sync(
                &mut sim,
                &mut out,
                Token(200 + idx),
                Op::FileRead {
                    slot: FileSlot(0),
                    buf: rbuf,
                    off: 0,
                    len: 4096,
                    file_off: off,
                },
            );
            if n < 4096 {
                break;
            }
            let f = sim.buf(rbuf);
            let magic = u32::from_le_bytes([f[0], f[1], f[2], f[3]]);
            let plen = u32::from_le_bytes([f[4], f[5], f[6], f[7]]) as usize;
            let seq = u64::from_le_bytes([f[8], f[9], f[10], f[11], f[12], f[13], f[14], f[15]]);
            let exp = u64::from_le_bytes([f[16], f[17], f[18], f[19], f[20], f[21], f[22], f[23]]);
            if magic != WAL_MAGIC || HEADER_LEN + plen > f.len() {
                break;
            }
            if wal_crc(seq, &f[HEADER_LEN..HEADER_LEN + plen]) != exp {
                break;
            }
            rec_cnt += 1;
            off += 4096;
        }
        let _ = drive_one_sync(
            &mut sim,
            &mut out,
            Token(300),
            Op::FileClose { slot: FileSlot(0) },
        );
        (committed_cnt, rec_cnt)
    });

    assert!(committed >= 1 && recovered >= committed);
}

#[test]
fn sealed_simdisk_wal_recovery() {
    run_simdisk_wal_recovery_sealed();
}
