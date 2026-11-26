// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! File slots, open flags, `statx` size decoding, and shared validation.

use crate::io::{Limits, Op, Token, EBADF, EBUSY, EINVAL};

mod open_flags;
mod validate;
pub(crate) use validate::check_unix_path;

/// Index into the fixed file table (`0..limits.files_max`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileSlot(pub(crate) u32);

impl FileSlot {
    /// Fixed file slot `index`.
    #[inline]
    pub const fn new(index: u32) -> FileSlot {
        FileSlot(index)
    }

    /// The slot's index in the fixed file table.
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Portable open flags mapped to kernel values per platform and architecture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenFlags {
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) create: bool,
    pub(crate) exclusive: bool,
    pub(crate) empty: bool,
    pub(crate) direct: bool,
    pub(crate) directory: bool,
    pub(crate) dsync: bool,
}

/// Byte size of a Linux `struct statx` buffer.
pub const STATX_SIZE: usize = 256;

/// Read `stx_size` from a `FileStatus` output buffer in Linux `struct statx` layout.
pub fn file_size(buf: &[u8]) -> u64 {
    assert!(buf.len() >= STATX_SIZE, "statx buffer must be >= 256 bytes");
    let mut size = [0u8; 8];
    size.copy_from_slice(&buf[40..48]);
    u64::from_le_bytes(size)
}

impl Op {
    /// Whether this operation is one of the file operations allowed in a chain.
    pub fn is_file_op(&self) -> bool {
        matches!(
            self,
            Op::FileOpen { .. }
                | Op::FileClose { .. }
                | Op::FileRead { .. }
                | Op::FileWrite { .. }
                | Op::FileSync { .. }
                | Op::FileReserve { .. }
                | Op::FilePunch { .. }
                | Op::FileStatus { .. }
                | Op::FileRename { .. }
                | Op::FileUnlink { .. }
                | Op::FileMkdir { .. }
        )
    }
}

#[derive(Clone, Copy)]
pub(super) struct SlotEntry {
    has_file: bool,
    opens: u16,
    closes: u16,
    direct: bool,
    next_direct: bool,
    sim_gen: u32,
    sim_active: bool,
    sim_can_open: bool,
    sim_direct: bool,
}

impl SlotEntry {
    const EMPTY: Self = Self {
        has_file: false,
        opens: 0,
        closes: 0,
        direct: false,
        next_direct: false,
        sim_gen: 0,
        sim_active: false,
        sim_can_open: true,
        sim_direct: false,
    };

    #[inline]
    fn is_free_for_open(self) -> bool {
        !self.has_file && self.opens == 0 && self.closes == 0
    }

    #[inline]
    fn is_usable(self) -> bool {
        (self.has_file || self.opens > 0) && self.closes == 0
    }
}

/// Shared file slot state and pre-submission validator used by both `UringIo` and `Engine<W>`.
pub(crate) struct FileTable {
    slots: Box<[SlotEntry]>,
    gen: u32,
}

impl FileTable {
    pub(crate) fn new(files_max: u32) -> Self {
        Self {
            slots: vec![SlotEntry::EMPTY; files_max as usize].into_boxed_slice(),
            gen: 0,
        }
    }

    pub(crate) fn has_file(&self, slot: FileSlot) -> bool {
        self.slots.get(slot.0 as usize).is_some_and(|s| s.has_file)
    }

    #[cfg(any(test, feature = "sim"))]
    pub(crate) fn reset(&mut self) {
        self.slots.fill(SlotEntry::EMPTY);
        self.gen = 0;
    }

    pub(crate) fn on_open_done(&mut self, slot: FileSlot, ok: bool, direct: bool) {
        if let Some(s) = self.slots.get_mut(slot.0 as usize) {
            s.opens = s.opens.saturating_sub(1);
            if ok {
                s.has_file = true;
                s.direct = direct;
            } else if !s.has_file && s.opens == 0 && s.closes == 0 {
                s.direct = false;
                s.next_direct = false;
            }
        }
    }

    pub(crate) fn on_close_done(&mut self, slot: FileSlot, ran: bool) {
        if let Some(s) = self.slots.get_mut(slot.0 as usize) {
            s.closes = s.closes.saturating_sub(1);
            if ran {
                s.has_file = false;
                if s.opens > 0 && s.closes == 0 {
                    s.direct = s.next_direct;
                } else if s.opens == 0 && s.closes == 0 {
                    s.direct = false;
                    s.next_direct = false;
                }
            } else if !s.has_file && s.opens == 0 && s.closes == 0 {
                s.direct = false;
                s.next_direct = false;
            }
        }
    }

    /// Validate a single file op; if accepted, record its submission effect on the slot table.
    pub(crate) fn check_op(&mut self, op: &Op, pool: &[u8], limits: &Limits) -> Result<(), i32> {
        match *op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => {
                let s = self.slots.get_mut(slot.0 as usize).ok_or(-EBADF)?;
                validate::check_path(pool, limits, path_buf, path_off, path_len)?;
                if !s.is_free_for_open() {
                    return Err(-EBUSY);
                }
                s.opens += 1;
                s.direct = flags.direct;
                s.next_direct = flags.direct;
                Ok(())
            }
            Op::FileClose { slot } => {
                let s = self.slots.get(slot.0 as usize).ok_or(-EBADF)?;
                if !s.is_usable() {
                    return Err(-EBADF);
                }
                self.slots[slot.0 as usize].closes += 1;
                Ok(())
            }
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::ArmedRecvZc { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => {
                validate::check_non_lifecycle(&self.slots, op, pool, limits, false)
            }
        }
    }

    /// Validate an entire chain before any member reaches a kernel or world.
    pub(crate) fn check_chain(
        &mut self,
        ops: &[(Token, Op)],
        pool: &[u8],
        limits: &Limits,
    ) -> Result<(), (usize, i32)> {
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            for s in self.slots.iter_mut() {
                s.sim_gen = 0;
            }
            self.gen = 1;
        }
        let g = self.gen;
        for (idx, (_, op)) in ops.iter().enumerate() {
            if !op.is_file_op() {
                return Err((idx, -EINVAL));
            }
            self.simulate_chain_op(op, pool, limits, g)
                .map_err(|e| (idx, e))?;
        }
        self.commit_chain(ops);
        Ok(())
    }

    /// Checks one chain member against the slot states the members before
    /// it leave behind, and applies its effect to those simulated states.
    fn simulate_chain_op(
        &mut self,
        op: &Op,
        pool: &[u8],
        limits: &Limits,
        g: u32,
    ) -> Result<(), i32> {
        match *op {
            Op::FileOpen {
                slot,
                path_buf,
                path_off,
                path_len,
                flags,
            } => {
                let can_open = self.sync_sim(slot, g)?.sim_can_open;
                validate::check_path(pool, limits, path_buf, path_off, path_len)?;
                if !can_open {
                    Err(-EBUSY)
                } else {
                    let s = &mut self.slots[slot.0 as usize];
                    s.sim_can_open = false;
                    s.sim_active = true;
                    s.sim_direct = flags.direct;
                    Ok(())
                }
            }
            Op::FileClose { slot } => {
                let active = self.sync_sim(slot, g)?.sim_active;
                if !active {
                    Err(-EBADF)
                } else {
                    let s = &mut self.slots[slot.0 as usize];
                    s.sim_active = false;
                    s.sim_can_open = true;
                    s.sim_direct = false;
                    Ok(())
                }
            }
            Op::Connect { .. }
            | Op::ConnectUnix { .. }
            | Op::ConnectUdp { .. }
            | Op::Accept { .. }
            | Op::ArmedAccept { .. }
            | Op::ArmedRecv { .. }
            | Op::ArmedRecvZc { .. }
            | Op::Send { .. }
            | Op::SendZc { .. }
            | Op::Recv { .. }
            | Op::RecvNow { .. }
            | Op::Shutdown { .. }
            | Op::Close { .. }
            | Op::AwaitWake
            | Op::ReadFile { .. }
            | Op::Timeout { .. }
            | Op::Cancel { .. }
            | Op::FileRead { .. }
            | Op::FileWrite { .. }
            | Op::FileSync { .. }
            | Op::FileReserve { .. }
            | Op::FilePunch { .. }
            | Op::FileStatus { .. }
            | Op::FileRename { .. }
            | Op::FileUnlink { .. }
            | Op::FileMkdir { .. } => {
                if let Some(slot) = validate::slot_of(op) {
                    let _: &SlotEntry = self.sync_sim(slot, g)?;
                }
                validate::check_non_lifecycle(&self.slots, op, pool, limits, true)
            }
        }
    }

    /// Records the opens and closes of a chain that passed validation.
    fn commit_chain(&mut self, ops: &[(Token, Op)]) {
        for (_, op) in ops {
            match *op {
                Op::FileOpen { slot, flags, .. } => {
                    let s = &mut self.slots[slot.0 as usize];
                    if s.is_free_for_open() {
                        s.direct = flags.direct;
                    }
                    s.opens += 1;
                    s.next_direct = flags.direct;
                }
                Op::FileClose { slot } => {
                    self.slots[slot.0 as usize].closes += 1;
                }
                Op::Connect { .. }
                | Op::ConnectUnix { .. }
                | Op::ConnectUdp { .. }
                | Op::Accept { .. }
                | Op::ArmedAccept { .. }
                | Op::ArmedRecv { .. }
                | Op::ArmedRecvZc { .. }
                | Op::Send { .. }
                | Op::SendZc { .. }
                | Op::Recv { .. }
                | Op::RecvNow { .. }
                | Op::Shutdown { .. }
                | Op::Close { .. }
                | Op::AwaitWake
                | Op::ReadFile { .. }
                | Op::Timeout { .. }
                | Op::Cancel { .. }
                | Op::FileRead { .. }
                | Op::FileWrite { .. }
                | Op::FileSync { .. }
                | Op::FileReserve { .. }
                | Op::FilePunch { .. }
                | Op::FileStatus { .. }
                | Op::FileRename { .. }
                | Op::FileUnlink { .. }
                | Op::FileMkdir { .. } => {}
            }
        }
    }

    fn sync_sim(&mut self, slot: FileSlot, g: u32) -> Result<&SlotEntry, i32> {
        let s = self.slots.get_mut(slot.0 as usize).ok_or(-EBADF)?;
        if s.sim_gen != g {
            s.sim_gen = g;
            s.sim_active = s.is_usable();
            s.sim_can_open = s.is_free_for_open();
            s.sim_direct = s.direct;
        }
        Ok(s)
    }
}
