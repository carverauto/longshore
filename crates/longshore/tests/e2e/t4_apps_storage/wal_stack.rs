// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation stack-only WAL driver for `t4_apps_storage::wal_lifecycle`.

use longshore::checksum::Crc64Nvme;
use longshore::io::files::file_size;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token, ENOENT};
use longshore::sim::SimIo;

const WAL_MAGIC: u32 = 0x5741_4C31;
const HEADER_LEN: usize = 24;

pub(super) struct StackWal {
    block_size: u32,
    write_buf: BufId,
    read_buf: BufId,
    path_buf: BufId,
    scratch_buf: BufId,
    pub cursor: u64,
    pub next_seq: u64,
    next_tok: u64,
}

impl StackWal {
    pub(super) fn new(lim: Limits, block_size: u32) -> Self {
        Self {
            block_size,
            write_buf: lim.aligned(0),
            read_buf: lim.aligned(1),
            path_buf: lim.small(0),
            scratch_buf: lim.small(1),
            cursor: 0,
            next_seq: 1,
            next_tok: 0x5741_0000,
        }
    }

    fn tok(&mut self) -> Token {
        let t = Token(self.next_tok);
        self.next_tok = self.next_tok.wrapping_add(1);
        t
    }

    fn run_one(&mut self, sim: &mut SimIo, out: &mut Completions, op: Op) -> i32 {
        let tok = self.tok();
        sim.submit(tok, op).unwrap();
        for _ in 0..2048 {
            sim.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == tok {
                    return c.result;
                }
            }
        }
        panic!("timed out waiting for {tok:?}");
    }

    fn write_path(&self, sim: &mut SimIo, buf: BufId, seg: Option<u32>) -> u32 {
        let dst = sim.buf(buf);
        dst[..4].copy_from_slice(b"/wal");
        let Some(mut n) = seg else {
            dst[4] = 0;
            return 5;
        };
        let mut d = [b'0'; 4];
        for ch in d.iter_mut().rev() {
            *ch = b'0' + (n % 10) as u8;
            n /= 10;
        }
        dst[4..9].copy_from_slice(b"/wal_");
        dst[9..13].copy_from_slice(&d);
        dst[13..18].copy_from_slice(b".log\0");
        18
    }

    pub(super) fn open_seg(
        &mut self,
        sim: &mut SimIo,
        out: &mut Completions,
        seg: u32,
        create: bool,
        reserve: bool,
    ) -> i32 {
        let path_len = self.write_path(sim, self.path_buf, Some(seg));
        let flags = OpenFlags {
            read: true,
            write: true,
            create,
            direct: true,
            ..Default::default()
        };
        let res = self.run_one(
            sim,
            out,
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: self.path_buf,
                path_off: 0,
                path_len,
                flags,
            },
        );
        if res >= 0 && reserve {
            let op = Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: u64::from(self.block_size) * 16,
                keep_size: false,
            };
            return self.run_one(sim, out, op);
        }
        res
    }

    pub(super) fn close_seg(&mut self, sim: &mut SimIo, out: &mut Completions) -> i32 {
        self.run_one(sim, out, Op::FileClose { slot: FileSlot(0) })
    }

    pub(super) fn append_chained(
        &mut self,
        sim: &mut SimIo,
        out: &mut Completions,
        payload: &[u8],
    ) -> (i32, i32) {
        let block = self.block_size as usize;
        let (seq, crc) = (self.next_seq, frame_crc(self.next_seq, payload));
        let b = sim.buf(self.write_buf);
        b[..block].fill(0);
        b[0..4].copy_from_slice(&WAL_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        b[8..16].copy_from_slice(&seq.to_le_bytes());
        b[16..24].copy_from_slice(&crc.to_le_bytes());
        b[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
        let (tw, ts) = (self.tok(), self.tok());
        let w_op = Op::FileWrite {
            slot: FileSlot(0),
            buf: self.write_buf,
            off: 0,
            len: self.block_size,
            file_off: self.cursor,
        };
        let s_op = Op::FileSync {
            slot: FileSlot(0),
            datasync: true,
        };
        sim.submit_chain(&[(tw, w_op), (ts, s_op)]).unwrap();
        let (mut w, mut s, mut gw, mut gs) = (-1, -1, false, false);
        for _ in 0..2048 {
            sim.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == tw {
                    (w, gw) = (c.result, true);
                } else if c.token == ts {
                    (s, gs) = (c.result, true);
                }
            }
            if gw && gs {
                break;
            }
        }
        assert!(gw && gs, "append_chained timed out");
        if w == self.block_size as i32 && s == 0 {
            self.cursor += u64::from(self.block_size);
            self.next_seq += 1;
        }
        (w, s)
    }

    pub(super) fn rename_seg(
        &mut self,
        sim: &mut SimIo,
        out: &mut Completions,
        from: u32,
        to: u32,
    ) -> i32 {
        let old_len = self.write_path(sim, self.path_buf, Some(from));
        let new_len = self.write_path(sim, self.scratch_buf, Some(to));
        let op = Op::FileRename {
            old_buf: self.path_buf,
            old_off: 0,
            old_len,
            new_buf: self.scratch_buf,
            new_off: 0,
            new_len,
        };
        let res = self.run_one(sim, out, op);
        if res < 0 {
            return res;
        }
        self.sync_dir(sim, out)
    }

    pub(super) fn sync_dir(&mut self, sim: &mut SimIo, out: &mut Completions) -> i32 {
        let dir_len = self.write_path(sim, self.path_buf, None);
        let (t1, t2, t3) = (self.tok(), self.tok(), self.tok());
        let o_op = Op::FileOpen {
            slot: FileSlot(1),
            path_buf: self.path_buf,
            path_off: 0,
            path_len: dir_len,
            flags: OpenFlags {
                read: true,
                directory: true,
                ..Default::default()
            },
        };
        let s_op = Op::FileSync {
            slot: FileSlot(1),
            datasync: false,
        };
        let c_op = Op::FileClose { slot: FileSlot(1) };
        sim.submit_chain(&[(t1, o_op), (t2, s_op), (t3, c_op)])
            .unwrap();
        let (mut sync_res, mut done) = (0, false);
        for _ in 0..2048 {
            sim.tick(true, out).unwrap();
            while let Some(c) = out.pop() {
                if c.token == t2 {
                    sync_res = c.result;
                } else if c.token == t3 {
                    done = true;
                }
            }
            if done {
                break;
            }
        }
        assert!(done, "dir sync chain timed out");
        sync_res
    }

    pub(super) fn recover(
        &mut self,
        sim: &mut SimIo,
        out: &mut Completions,
        max_segs: u32,
        on_seq: &mut impl FnMut(u64),
    ) -> (u32, u64, bool) {
        let (mut valid, mut last_seq, mut fault_stop) = (0u32, 0u64, false);
        let block = u64::from(self.block_size);
        for seg in 1..=max_segs {
            let path_len = self.write_path(sim, self.path_buf, Some(seg));
            let st = self.run_one(
                sim,
                out,
                Op::FileStatus {
                    path_buf: self.path_buf,
                    path_off: 0,
                    path_len,
                    out_buf: self.scratch_buf,
                    out_off: 0,
                },
            );
            if st == -ENOENT {
                continue;
            }
            assert!(st >= 0, "unexpected FileStatus error {st}");
            let size = file_size(sim.buf(self.scratch_buf));
            if self.open_seg(sim, out, seg, false, false) < 0 {
                continue;
            }
            let mut off = 0u64;
            while off + block <= size {
                let res = self.run_one(
                    sim,
                    out,
                    Op::FileRead {
                        slot: FileSlot(0),
                        buf: self.read_buf,
                        off: 0,
                        len: self.block_size,
                        file_off: off,
                    },
                );
                if res < 0 {
                    fault_stop = true;
                    break;
                }
                match check_frame(&sim.buf(self.read_buf)[..res as usize]) {
                    Ok(None) => break,
                    Err(()) => {
                        fault_stop = true;
                        break;
                    }
                    Ok(Some(seq)) => {
                        on_seq(seq);
                        valid += 1;
                        last_seq = seq;
                        off += block;
                    }
                }
            }
            let _ = self.close_seg(sim, out);
        }
        if last_seq >= self.next_seq {
            self.next_seq = last_seq + 1;
        }
        (valid, last_seq, fault_stop)
    }
}

fn check_frame(f: &[u8]) -> Result<Option<u64>, ()> {
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
    Ok(Some(seq))
}

fn frame_crc(seq: u64, payload: &[u8]) -> u64 {
    let mut c = Crc64Nvme::new();
    c.update(&seq.to_le_bytes());
    c.update(&(payload.len() as u32).to_le_bytes());
    c.update(payload);
    c.finish()
}
