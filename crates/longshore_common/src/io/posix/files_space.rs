// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Space reservation, hole punching, `statx`, and namespace calls for [`Posix`].

#[cfg(target_os = "linux")]
use super::super::abi_linux::*;
#[cfg(target_os = "macos")]
use super::super::abi_macos::*;
use super::super::errno::{self, to_linux_errno};
use super::super::Posix;
use crate::io::files::STATX_SIZE;
use crate::io::FileSlot;
use core::ffi::c_char;
#[cfg(target_os = "linux")]
use core::ffi::{c_int, c_void};

impl Posix {
    pub(super) fn file_reserve(
        &self,
        slot: FileSlot,
        file_off: u64,
        len: u64,
        keep_size: bool,
    ) -> i32 {
        let fd = self.file_fds[slot.0 as usize];
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        if len == 0 {
            return -errno::linux::EINVAL;
        }
        #[cfg(target_os = "linux")]
        {
            let Ok(off) = i64::try_from(file_off) else {
                return -errno::linux::EINVAL;
            };
            let Ok(l) = i64::try_from(len) else {
                return -errno::linux::EFBIG;
            };
            let mode = if keep_size { FALLOC_FL_KEEP_SIZE } else { 0 };
            loop {
                if unsafe { fallocate(fd, mode, off, l) } == 0 {
                    return 0;
                }
                let e = os_errno();
                if e != OS_EINTR {
                    return to_linux_errno(e);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let Some(target) = file_off.checked_add(len) else {
                return -errno::linux::EFBIG;
            };
            let Ok(target_i64) = i64::try_from(target) else {
                return -errno::linux::EFBIG;
            };
            let mut st = stat::default();
            if unsafe { fstat(fd, &mut st) } < 0 {
                return to_linux_errno(os_errno());
            }
            let cur_size = st.st_size.max(0);
            if target_i64 > cur_size {
                let mut fs = fstore_t {
                    fst_flags: F_ALLOCATECONTIG,
                    fst_posmode: F_PEOFPOSMODE,
                    fst_offset: 0,
                    fst_length: target_i64 - cur_size,
                    fst_bytesalloc: 0,
                };
                if unsafe { fcntl(fd, F_PREALLOCATE, &mut fs) } < 0 {
                    fs.fst_flags = F_ALLOCATEALL;
                    if unsafe { fcntl(fd, F_PREALLOCATE, &mut fs) } < 0 {
                        let mapped = to_linux_errno(os_errno());
                        if mapped != -errno::linux::EOPNOTSUPP {
                            return mapped;
                        }
                    }
                }
                if !keep_size {
                    loop {
                        if unsafe { ftruncate(fd, target_i64) } == 0 {
                            break;
                        }
                        let e = os_errno();
                        if e != OS_EINTR {
                            return to_linux_errno(e);
                        }
                    }
                }
            }
            0
        }
    }

    pub(super) fn file_punch(&self, slot: FileSlot, file_off: u64, len: u64) -> i32 {
        let fd = self.file_fds[slot.0 as usize];
        if fd < 0 {
            return -errno::linux::EBADF;
        }
        if len == 0 {
            return -errno::linux::EINVAL;
        }
        #[cfg(target_os = "linux")]
        {
            let Ok(off) = i64::try_from(file_off) else {
                return -errno::linux::EINVAL;
            };
            let Ok(l) = i64::try_from(len) else {
                return -errno::linux::EFBIG;
            };
            loop {
                if unsafe { fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, off, l) } == 0
                {
                    return 0;
                }
                let e = os_errno();
                if e != OS_EINTR {
                    return to_linux_errno(e);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let Ok(off) = i64::try_from(file_off) else {
                return -errno::linux::EINVAL;
            };
            let Ok(l) = i64::try_from(len) else {
                return -errno::linux::EFBIG;
            };
            let mut ph = fpunchhole_t {
                fp_flags: 0,
                reserved: 0,
                fp_offset: off,
                fp_length: l,
            };
            if unsafe { fcntl(fd, F_PUNCHHOLE, &mut ph) } < 0 {
                return to_linux_errno(os_errno());
            }
            0
        }
    }

    pub(super) fn file_status(
        pool: &mut [u8],
        path_range: core::ops::Range<usize>,
        out_range: core::ops::Range<usize>,
    ) -> i32 {
        let mut raw = [0u8; STATX_SIZE];
        let path: *const c_char = pool[path_range].as_ptr().cast();
        #[cfg(target_os = "linux")]
        {
            let r = unsafe {
                syscall(
                    SYS_STATX,
                    AT_FDCWD,
                    path,
                    0 as c_int,
                    STATX_BASIC_STATS,
                    raw.as_mut_ptr().cast::<c_void>(),
                )
            };
            if r < 0 {
                return to_linux_errno(os_errno());
            }
        }
        #[cfg(target_os = "macos")]
        {
            let mut st = stat::default();
            if unsafe { stat(path, &mut st) } < 0 {
                return to_linux_errno(os_errno());
            }
            raw[0..4].copy_from_slice(&0x07ffu32.to_le_bytes());
            raw[4..8].copy_from_slice(&u32::try_from(st.st_blksize).unwrap_or(0).to_le_bytes());
            raw[16..20].copy_from_slice(&u32::from(st.st_nlink).to_le_bytes());
            raw[20..24].copy_from_slice(&st.st_uid.to_le_bytes());
            raw[24..28].copy_from_slice(&st.st_gid.to_le_bytes());
            raw[28..30].copy_from_slice(&st.st_mode.to_le_bytes());
            raw[32..40].copy_from_slice(&st.st_ino.to_le_bytes());
            raw[40..48].copy_from_slice(&u64::try_from(st.st_size).unwrap_or(0).to_le_bytes());
            raw[48..56].copy_from_slice(&u64::try_from(st.st_blocks).unwrap_or(0).to_le_bytes());
        }
        pool[out_range].copy_from_slice(&raw);
        0
    }

    pub(super) fn file_rename(old_path: *const c_char, new_path: *const c_char) -> i32 {
        #[cfg(target_os = "macos")]
        let r = unsafe { rename(old_path, new_path) };
        #[cfg(target_os = "linux")]
        let r = unsafe { renameat(AT_FDCWD, old_path, AT_FDCWD, new_path) };
        if r < 0 {
            to_linux_errno(os_errno())
        } else {
            0
        }
    }

    pub(super) fn file_unlink(path: *const c_char) -> i32 {
        #[cfg(target_os = "macos")]
        let r = unsafe { unlink(path) };
        #[cfg(target_os = "linux")]
        let r = unsafe { unlinkat(AT_FDCWD, path, 0) };
        if r < 0 {
            to_linux_errno(os_errno())
        } else {
            0
        }
    }

    pub(super) fn file_mkdir(path: *const c_char) -> i32 {
        #[cfg(target_os = "macos")]
        let r = unsafe { mkdir(path, 0o777) };
        #[cfg(target_os = "linux")]
        let r = unsafe { mkdirat(AT_FDCWD, path, 0o777) };
        if r < 0 {
            to_linux_errno(os_errno())
        } else {
            0
        }
    }
}
