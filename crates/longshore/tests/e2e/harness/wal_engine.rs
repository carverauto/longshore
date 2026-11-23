// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation mini WAL state machine over generic `longshore::io::Io`.

use super::io_driver::{drive_until_stack, submit_and_wait_one_stack};
use longshore::checksum::Crc64Nvme;
use longshore::io::files::file_size;
use longshore::io::{BufId, Completions, FileSlot, Io, Op, OpenFlags, Token, ENOENT};
use std::io;

const WAL_MAGIC: u32 = 0x5741_4C31;
const HEADER_LEN: usize = 24;

#[derive(Clone, Copy, Debug)]
pub struct WalConfig {
    pub file_slot: FileSlot,
    pub dir_slot: FileSlot,
    pub write_buf: BufId,
    pub read_buf: BufId,
    pub path_buf: BufId,
    pub scratch_buf: BufId,
    pub block_size: u32,
    pub segment_bytes: u64,
    pub direct: bool,
    base_dir: [u8; 128],
    base_dir_len: usize,
}

impl WalConfig {
    pub fn new(dir: &str, block_size: u32) -> Self {
        let raw = dir.trim_end_matches('/').as_bytes();
        assert!(raw.len() <= 128, "WAL base_dir exceeds 128 bytes");
        let mut base_dir = [0u8; 128];
        base_dir[..raw.len()].copy_from_slice(raw);
        Self {
            file_slot: FileSlot(0),
            dir_slot: FileSlot(1),
            write_buf: BufId(0),
            read_buf: BufId(1),
            path_buf: BufId(2),
            scratch_buf: BufId(3),
            block_size,
            segment_bytes: u64::from(block_size) * 16,
            direct: true,
            base_dir,
            base_dir_len: raw.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalRecord<'a> {
    pub seg_idx: u32,
    pub offset: u64,
    pub seq: u64,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalStats {
    pub segments_found: u32,
    pub valid_records: u32,
    pub last_seq: u64,
    pub stopped_on_corrupt: bool,
    pub stopped_on_read_error: bool,
}

pub struct WalEngine {
    cfg: WalConfig,
    pub cursor: u64,
    pub next_seq: u64,
    next_token: u64,
}

impl WalEngine {
    pub fn new(cfg: WalConfig) -> Self {
        Self {
            cfg,
            cursor: 0,
            next_seq: 1,
            next_token: 0x5741_0000,
        }
    }
    pub fn reset_after_power_loss(&mut self) {
        self.cursor = 0;
    }
    fn tok(&mut self) -> Token {
        let t = Token(self.next_token);
        self.next_token = self.next_token.wrapping_add(1);
        t
    }
    fn run<IO: Io>(&mut self, io: &mut IO, out: &mut Completions, op: Op) -> io::Result<i32> {
        let tok = self.tok();
        Ok(submit_and_wait_one_stack(io, out, tok, op, 10_000)?.result)
    }

    fn write_path<IO: Io>(&self, io: &mut IO, buf: BufId, seg: Option<u32>) -> u32 {
        let (dst, len) = (io.buf(buf), self.cfg.base_dir_len);
        if len > 0 {
            dst[..len].copy_from_slice(&self.cfg.base_dir[..len]);
        }
        let Some(mut n) = seg else {
            return if len == 0 {
                dst[0..2].copy_from_slice(b"/\0");
                2
            } else {
                dst[len] = 0;
                (len + 1) as u32
            };
        };
        let mut d = [b'0'; 4];
        for ch in d.iter_mut().rev() {
            *ch = b'0' + (n % 10) as u8;
            n /= 10;
        }
        dst[len..len + 5].copy_from_slice(b"/wal_");
        dst[len + 5..len + 9].copy_from_slice(&d);
        dst[len + 9..len + 14].copy_from_slice(b".log\0");
        (len + 14) as u32
    }

    pub fn open_segment<IO: Io>(
        &mut self,
        io: &mut IO,
        out: &mut Completions,
        seg: u32,
        create: bool,
        reserve: bool,
    ) -> io::Result<i32> {
        let (slot, path_buf) = (self.cfg.file_slot, self.cfg.path_buf);
        let path_len = self.write_path(io, path_buf, Some(seg));
        let flags = OpenFlags {
            read: true,
            write: true,
            create,
            direct: self.cfg.direct,
            ..Default::default()
        };
        let op = Op::FileOpen {
            slot,
            path_buf,
            path_off: 0,
            path_len,
            flags,
        };
        let res = self.run(io, out, op)?;
        if res >= 0 && reserve && self.cfg.segment_bytes > 0 {
            let reserve_op = Op::FileReserve {
                slot,
                file_off: 0,
                len: self.cfg.segment_bytes,
                keep_size: false,
            };
            return self.run(io, out, reserve_op);
        }
        Ok(res)
    }

    pub fn close_segment<IO: Io>(&mut self, io: &mut IO, out: &mut Completions) -> io::Result<i32> {
        let slot = self.cfg.file_slot;
        self.run(io, out, Op::FileClose { slot })
    }

    pub fn append_chained<IO: Io>(
        &mut self,
        io: &mut IO,
        out: &mut Completions,
        payload: &[u8],
    ) -> io::Result<(i32, i32)> {
        let block = self.cfg.block_size as usize;
        assert!(
            HEADER_LEN + payload.len() <= block,
            "WAL payload exceeds block"
        );
        let (seq, crc) = (self.next_seq, frame_crc(self.next_seq, payload));
        let b = io.buf(self.cfg.write_buf);
        b[..block].fill(0);
        b[0..4].copy_from_slice(&WAL_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        b[8..16].copy_from_slice(&seq.to_le_bytes());
        b[16..24].copy_from_slice(&crc.to_le_bytes());
        b[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
        let (slot, buf, file_off, len) = (
            self.cfg.file_slot,
            self.cfg.write_buf,
            self.cursor,
            block as u32,
        );
        let (t_w, t_s) = (self.tok(), self.tok());
        let w_op = Op::FileWrite {
            slot,
            buf,
            off: 0,
            len,
            file_off,
        };
        let s_op = Op::FileSync {
            slot,
            datasync: true,
        };
        io.submit_chain(&[(t_w, w_op), (t_s, s_op)])
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "Busy"))?;
        let (mut w, mut s) = (0, 0);
        let _: usize = drive_until_stack::<IO, 32>(io, out, 10_000, |v| {
            let cw = v.iter().find(|c| c.token == t_w);
            let cs = v.iter().find(|c| c.token == t_s);
            if let (Some(write), Some(sync)) = (cw, cs) {
                (w, s) = (write.result, sync.result);
                return true;
            }
            false
        })?;
        if w == i32::try_from(block).unwrap() && s == 0 {
            self.cursor += block as u64;
            self.next_seq += 1;
        }
        Ok((w, s))
    }

    pub fn probe_segment<IO: Io>(
        &mut self,
        io: &mut IO,
        out: &mut Completions,
        seg: u32,
    ) -> io::Result<Option<u64>> {
        let (path_buf, out_buf) = (self.cfg.path_buf, self.cfg.scratch_buf);
        let path_len = self.write_path(io, path_buf, Some(seg));
        let op = Op::FileStatus {
            path_buf,
            path_off: 0,
            path_len,
            out_buf,
            out_off: 0,
        };
        let res = self.run(io, out, op)?;
        if res == -ENOENT {
            Ok(None)
        } else if res < 0 {
            Err(io::Error::from_raw_os_error(-res))
        } else {
            Ok(Some(file_size(io.buf(out_buf))))
        }
    }

    pub fn rename_segment<IO: Io>(
        &mut self,
        io: &mut IO,
        out: &mut Completions,
        from: u32,
        to: u32,
        sync_dir: bool,
    ) -> io::Result<i32> {
        let (old_buf, new_buf, slot) = (self.cfg.path_buf, self.cfg.scratch_buf, self.cfg.dir_slot);
        let (old_len, new_len) = (
            self.write_path(io, old_buf, Some(from)),
            self.write_path(io, new_buf, Some(to)),
        );
        let op = Op::FileRename {
            old_buf,
            old_off: 0,
            old_len,
            new_buf,
            new_off: 0,
            new_len,
        };
        let res = self.run(io, out, op)?;
        if res < 0 || !sync_dir {
            return Ok(res);
        }
        let path_len = self.write_path(io, old_buf, None);
        let flags = OpenFlags {
            read: true,
            directory: true,
            ..Default::default()
        };
        let (t1, t2, t3) = (self.tok(), self.tok(), self.tok());
        let o_op = Op::FileOpen {
            slot,
            path_buf: old_buf,
            path_off: 0,
            path_len,
            flags,
        };
        let s_op = Op::FileSync {
            slot,
            datasync: false,
        };
        let c_op = Op::FileClose { slot };
        io.submit_chain(&[(t1, o_op), (t2, s_op), (t3, c_op)])
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "Busy"))?;
        let mut s_res = 0;
        let _: usize = drive_until_stack::<IO, 32>(io, out, 10_000, |v| {
            if v.iter().any(|c| c.token == t3) {
                s_res = v.iter().find(|c| c.token == t2).map_or(0, |c| c.result);
                return true;
            }
            false
        })?;
        Ok(s_res)
    }

    pub fn recover<IO: Io>(
        &mut self,
        io: &mut IO,
        out: &mut Completions,
        max_segs: u32,
        on_rec: &mut impl FnMut(WalRecord<'_>),
    ) -> io::Result<WalStats> {
        let (mut stats, block, slot, buf) = (
            WalStats::default(),
            u64::from(self.cfg.block_size),
            self.cfg.file_slot,
            self.cfg.read_buf,
        );
        for seg in 1..=max_segs {
            let Some(size) = self.probe_segment(io, out, seg)? else {
                continue;
            };
            stats.segments_found += 1;
            if self.open_segment(io, out, seg, false, false)? < 0 {
                continue;
            }
            let mut off = 0u64;
            while off + block <= size {
                let op = Op::FileRead {
                    slot,
                    buf,
                    off: 0,
                    len: block as u32,
                    file_off: off,
                };
                let res = self.run(io, out, op)?;
                if res < 0 {
                    stats.stopped_on_read_error = true;
                    break;
                }
                let frame = &io.buf(buf)[..usize::try_from(res).unwrap()];
                match check_frame(frame) {
                    Ok(None) => break,
                    Err(()) => {
                        stats.stopped_on_corrupt = true;
                        break;
                    }
                    Ok(Some((seq, payload))) => {
                        on_rec(WalRecord {
                            seg_idx: seg,
                            offset: off,
                            seq,
                            payload,
                        });
                        stats.valid_records += 1;
                        stats.last_seq = seq;
                        off += block;
                    }
                }
            }
            let _: i32 = self.close_segment(io, out)?;
        }
        if stats.last_seq >= self.next_seq {
            self.next_seq = stats.last_seq + 1;
        }
        Ok(stats)
    }
}

fn check_frame(f: &[u8]) -> Result<Option<(u64, &[u8])>, ()> {
    if f.len() < HEADER_LEN {
        return Ok(None);
    }
    let magic = u32::from_le_bytes([f[0], f[1], f[2], f[3]]);
    let plen = u32::from_le_bytes([f[4], f[5], f[6], f[7]]) as usize;
    let seq = u64::from_le_bytes(f[8..16].try_into().expect("8B"));
    let exp = u64::from_le_bytes(f[16..24].try_into().expect("8B"));
    if magic == 0 && plen == 0 && seq == 0 {
        return Ok(None);
    }
    if magic != WAL_MAGIC
        || HEADER_LEN + plen > f.len()
        || frame_crc(seq, &f[HEADER_LEN..HEADER_LEN + plen]) != exp
    {
        return Err(());
    }
    Ok(Some((seq, &f[HEADER_LEN..HEADER_LEN + plen])))
}

fn frame_crc(seq: u64, payload: &[u8]) -> u64 {
    let mut c = Crc64Nvme::new();
    c.update(&seq.to_le_bytes());
    c.update(&(payload.len() as u32).to_le_bytes());
    c.update(payload);
    c.finish()
}
