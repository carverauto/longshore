// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Kernel ABI for io_uring, plus the libc symbols this crate needs.
//!
//! No `libc` crate: `std` already links the C library, so the symbols are
//! declared here directly. Values are transcribed from the uapi header and are
//! identical on x86-64 and AArch64 (io_uring syscalls use the unified numbers).

use core::ffi::{c_int, c_long, c_uint, c_void};

mod layout;
mod sock;
mod zcrx;
pub use layout::*;
pub use sock::*;
pub use zcrx::*;

/// System call number of `io_uring_setup`.
pub const SYS_IO_URING_SETUP: c_long = 425;
/// System call number of `io_uring_enter`.
pub const SYS_IO_URING_ENTER: c_long = 426;
/// System call number of `io_uring_register`.
pub const SYS_IO_URING_REGISTER: c_long = 427;

// io_uring_setup flags. Kernel that introduced each is noted.
/// The caller gives the completion queue's size in `cq_entries`.
pub const IORING_SETUP_CQSIZE: u32 = 1 << 3;
/// Clamp queue sizes over the kernel's limit instead of refusing them.
pub const IORING_SETUP_CLAMP: u32 = 1 << 4;
/// Keep submitting a batch past an entry that fails.
pub const IORING_SETUP_SUBMIT_ALL: u32 = 1 << 7; // 5.18
/// Do not interrupt the task to run completion work.
pub const IORING_SETUP_COOP_TASKRUN: u32 = 1 << 8; // 5.19
/// Only one task submits to the ring.
pub const IORING_SETUP_SINGLE_ISSUER: u32 = 1 << 12; // 6.0
/// Run completion work only when the task asks for completions. Needs
/// [`IORING_SETUP_SINGLE_ISSUER`].
pub const IORING_SETUP_DEFER_TASKRUN: u32 = 1 << 13; // 6.1

/// One mapping covers both the submission and the completion ring.
pub const IORING_FEAT_SINGLE_MMAP: u32 = 1 << 0;
/// The kernel keeps completions that do not fit the queue instead of dropping
/// them.
pub const IORING_FEAT_NODROP: u32 = 1 << 1;

/// Wait for completions.
pub const IORING_ENTER_GETEVENTS: u32 = 1 << 0;

/// Mapping offset of the submission ring.
pub const IORING_OFF_SQ_RING: i64 = 0;
/// Mapping offset of the completion ring.
pub const IORING_OFF_CQ_RING: i64 = 0x800_0000;
/// Mapping offset of the submission entries.
pub const IORING_OFF_SQES: i64 = 0x1000_0000;

/// Register buffers for fixed reads, writes and sends.
pub const IORING_REGISTER_BUFFERS: c_uint = 0; // 5.1
/// Register the fixed file table.
pub const IORING_REGISTER_FILES: c_uint = 2; // 5.1
/// Replace entries of the fixed file table.
pub const IORING_REGISTER_FILES_UPDATE: c_uint = 6; // 5.1
/// Register a ring of provided buffers.
pub const IORING_REGISTER_PBUF_RING: c_uint = 22; // 5.19
/// Unregister a ring of provided buffers.
pub const IORING_UNREGISTER_PBUF_RING: c_uint = 23; // 5.19
/// Set the range of the file table the kernel allocates slots from.
pub const IORING_REGISTER_FILE_ALLOC_RANGE: c_uint = 25; // 6.0

// Opcodes.
/// Do nothing, and complete.
pub const IORING_OP_NOP: u8 = 0;
/// Flush a file to storage.
pub const IORING_OP_FSYNC: u8 = 3;
/// Read into a registered buffer.
pub const IORING_OP_READ_FIXED: u8 = 4;
/// Write from a registered buffer.
pub const IORING_OP_WRITE_FIXED: u8 = 5;
/// Complete when a time has passed.
pub const IORING_OP_TIMEOUT: u8 = 11;
/// Remove a timeout.
pub const IORING_OP_TIMEOUT_REMOVE: u8 = 12;
/// Accept a connection.
pub const IORING_OP_ACCEPT: u8 = 13;
/// Cancel an operation in flight.
pub const IORING_OP_ASYNC_CANCEL: u8 = 14;
/// Connect a socket.
pub const IORING_OP_CONNECT: u8 = 16;
/// Reserve or release a file's space.
pub const IORING_OP_FALLOCATE: u8 = 17;
/// Open a file.
pub const IORING_OP_OPENAT: u8 = 18;
/// Close a file or socket.
pub const IORING_OP_CLOSE: u8 = 19;
/// Read a file's status.
pub const IORING_OP_STATX: u8 = 21;
/// Read into a buffer that need not be registered.
pub const IORING_OP_READ: u8 = 22;
/// Directory descriptor that stands for the current directory.
pub const AT_FDCWD: i32 = -100;
/// `AT_FDCWD` as `IORING_OP_RENAMEAT` takes its new directory: in `sqe.len`, a
/// `__u32` the kernel reads back as an `int` (`io_renameat_prep`).
#[expect(
    clippy::cast_sign_loss,
    reason = "renameat's new dirfd travels in the __u32 sqe.len; the kernel reads the bits back as int"
)]
pub const AT_FDCWD_SQE_LEN: u32 = AT_FDCWD as u32;
/// `unlinkat` flag: remove a directory.
pub const AT_REMOVEDIR: u32 = 0x200;
/// Open for reading.
pub const O_RDONLY: u32 = 0;
/// Open for writing.
pub const O_WRONLY: u32 = 1;
/// Open for reading and writing.
pub const O_RDWR: u32 = 2;
/// Create the file if it is missing.
pub const O_CREAT: u32 = 0o100;
/// With [`O_CREAT`]: refuse a file that exists.
pub const O_EXCL: u32 = 0o200;
/// Empty the file.
pub const O_TRUNC: u32 = 0o1000;
/// Bypass the page cache.
#[cfg(target_arch = "x86_64")]
pub const O_DIRECT: u32 = 0o0040000;
/// Refuse anything but a directory.
#[cfg(target_arch = "x86_64")]
pub const O_DIRECTORY: u32 = 0o0200000;
/// Refuse anything but a directory.
#[cfg(target_arch = "aarch64")]
pub const O_DIRECTORY: u32 = 0o0040000;
/// Bypass the page cache.
#[cfg(target_arch = "aarch64")]
pub const O_DIRECT: u32 = 0o0200000;
/// Complete each write only when its data is on stable storage. The value is
/// the one x86-64 and AArch64 share; other architectures differ, so it is
/// defined for these two only.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub const O_DSYNC: u32 = 0o10000;
/// Open for reading, with the descriptor closed on `exec`.
pub const O_RDONLY_CLOEXEC: u32 = 0o2000000;
/// Send on a socket.
pub const IORING_OP_SEND: u8 = 26;
/// Receive on a socket.
pub const IORING_OP_RECV: u8 = 27;
/// Shut a connection down in one or both directions.
pub const IORING_OP_SHUTDOWN: u8 = 34; // 5.11
/// Rename a path.
pub const IORING_OP_RENAMEAT: u8 = 35;
/// Remove a path.
pub const IORING_OP_UNLINKAT: u8 = 36;
/// Create a directory.
pub const IORING_OP_MKDIRAT: u8 = 37;
/// Open a socket.
pub const IORING_OP_SOCKET: u8 = 45; // 5.19
/// A command to the file's own driver, such as [`SOCKET_URING_OP_GETSOCKOPT`]
/// to a socket.
pub const IORING_OP_URING_CMD: u8 = 46; // 5.19
/// Send on a socket without copying the bytes.
pub const IORING_OP_SEND_ZC: u8 = 47; // 6.0

/// [`IORING_OP_URING_CMD`] command to a socket: read a socket option.
pub const SOCKET_URING_OP_GETSOCKOPT: u64 = 2; // 6.7

/// `sqe.fd` is an index into the registered file table.
pub const IOSQE_FIXED_FILE: u8 = 1 << 0;
/// Start the next entry only once this one has succeeded.
pub const IOSQE_IO_LINK: u8 = 1 << 2;
/// Start the next entry once this one has ended, however it ended.
pub const IOSQE_IO_HARDLINK: u8 = 1 << 3;
/// Take a buffer from the group `sqe.buf_index` names.
pub const IOSQE_BUFFER_SELECT: u8 = 1 << 5;

/// Keep accepting, with a completion per connection.
pub const IORING_ACCEPT_MULTISHOT: u16 = 1 << 0;
/// Keep receiving, with a completion per arrival.
pub const IORING_RECV_MULTISHOT: u16 = 1 << 1;
/// `sqe.file_index` value that has the kernel choose the slot.
pub const IORING_FILE_INDEX_ALLOC: u32 = !0u32;

/// Flush only the metadata that reading the data back needs.
pub const IORING_FSYNC_DATASYNC: u32 = 1 << 0;
/// Leave the file's size as it is.
pub const FALLOC_FL_KEEP_SIZE: u32 = 0x01;
/// Release the range's space. Needs [`FALLOC_FL_KEEP_SIZE`].
pub const FALLOC_FL_PUNCH_HOLE: u32 = 0x02;
/// Ask `statx` for everything `stat` gives.
pub const STATX_BASIC_STATS: u32 = 0x07ff;

/// The limit on open files.
pub const RLIMIT_NOFILE: c_int = 7;
/// The limit on locked memory, in bytes.
pub const RLIMIT_MEMLOCK: c_int = 8;
/// No limit.
pub const RLIM_INFINITY: u64 = !0u64;

// sqe.ioprio flags for send/recv.
/// Use the registered buffer that `sqe.buf_index` names.
pub const IORING_RECVSEND_FIXED_BUF: u16 = 1 << 2; // 6.0
/// Report in the notification whether the kernel copied the bytes.
pub const IORING_SEND_ZC_REPORT_USAGE: u16 = 1 << 3; // 6.2

/// The entry names a provided buffer, above [`IORING_CQE_BUFFER_SHIFT`].
pub const IORING_CQE_F_BUFFER: u32 = 1 << 0;
/// More entries will follow for the same submission.
pub const IORING_CQE_F_MORE: u32 = 1 << 1;
/// The entry is a zero-copy send's notification: the kernel has released the
/// buffer.
pub const IORING_CQE_F_NOTIF: u32 = 1 << 3;
/// The bit of `cqe.flags` where the provided buffer's id starts.
pub const IORING_CQE_BUFFER_SHIFT: u32 = 16;
/// Set in `cqe.res` of a notification CQE when the kernel copied the data
/// instead of sending it zero-copy (requires `IORING_SEND_ZC_REPORT_USAGE`).
pub const IORING_NOTIF_USAGE_ZC_COPIED: u32 = 1 << 31;

/// The flag bits a notification CQE carries in its `__s32` `res`, such as
/// [`IORING_NOTIF_USAGE_ZC_COPIED`] in bit 31.
#[expect(
    clippy::cast_sign_loss,
    reason = "a notification CQE's __s32 res holds flag bits, not a result; bit 31 is IORING_NOTIF_USAGE_ZC_COPIED"
)]
pub const fn notif_flags(res: i32) -> u32 {
    res as u32
}

/// `eventfd` flag: close the descriptor on `exec`.
pub const EFD_CLOEXEC: c_int = 0o2000000;
/// The clock that never goes back.
pub const CLOCK_MONOTONIC: c_int = 1;
/// The wall clock, counted from the Unix epoch.
pub const CLOCK_REALTIME: c_int = 0;

/// `sqe.fd` for registered-file index `index` under [`IOSQE_FIXED_FILE`].
#[expect(
    clippy::cast_possible_wrap,
    reason = "with IOSQE_FIXED_FILE the kernel reads the __s32 sqe.fd as an unsigned index into the registered file table"
)]
pub const fn fixed_fd(index: u32) -> i32 {
    index as i32
}

/// Mapped memory may be read.
pub const PROT_READ: c_int = 1;
/// Mapped memory may be written.
pub const PROT_WRITE: c_int = 2;
/// Changes are shared with the mapping's other users.
pub const MAP_SHARED: c_int = 0x01;
/// Changes stay with this mapping.
pub const MAP_PRIVATE: c_int = 0x02;
/// The mapping has no file behind it.
pub const MAP_ANONYMOUS: c_int = 0x20;
/// Fault the pages in at once.
pub const MAP_POPULATE: c_int = 0x8000;
/// What [`mmap`] returns when it fails.
pub const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

extern "C" {
    /// `syscall(2)`: the system call numbered `num`.
    pub fn syscall(num: c_long, ...) -> c_long;
    /// `mmap(2)`: map memory or a file.
    pub fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
    /// `munmap(2)`: undo a mapping.
    pub fn munmap(addr: *mut c_void, len: usize) -> c_int;
    /// `close(2)`: close a descriptor.
    pub fn close(fd: c_int) -> c_int;
    /// `eventfd(2)`: a descriptor to signal through.
    pub fn eventfd(initval: c_uint, flags: c_int) -> c_int;
    /// `write(2)`: write bytes to a descriptor.
    pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    /// `dup(2)`: another descriptor for the same file.
    pub fn dup(fd: c_int) -> c_int;
    /// `dup2(2)`: make `new` a descriptor for the file `old` names.
    #[cfg(feature = "fault-injection")]
    pub fn dup2(old: c_int, new: c_int) -> c_int;
    /// `clock_gettime(2)`: read a clock.
    pub fn clock_gettime(clk: c_int, ts: *mut timespec) -> c_int;
    /// `getrlimit(2)`: read a resource limit.
    pub fn getrlimit(resource: c_int, rlim: *mut rlimit) -> c_int;
}

/// Map portable [`crate::io::OpenFlags`] to Linux architecture-specific `openat` flags
/// without `O_CLOEXEC` (refused for registered `io_uring` fixed file slots).
pub fn open_flags_linux(f: crate::io::OpenFlags) -> u32 {
    let mut out = match (f.read, f.write) {
        (true, true) => O_RDWR,
        (false, true) => O_WRONLY,
        (true, false) | (false, false) => O_RDONLY,
    };
    if f.create {
        out |= O_CREAT;
    }
    if f.exclusive {
        out |= O_EXCL;
    }
    if f.empty {
        out |= O_TRUNC;
    }
    if f.direct {
        out |= O_DIRECT;
    }
    if f.directory {
        out |= O_DIRECTORY;
    }
    if f.dsync {
        out |= O_DSYNC;
    }
    out
}
