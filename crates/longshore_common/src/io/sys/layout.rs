// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The structures shared with the kernel, laid out as its headers lay them out.

use core::ffi::c_void;
use core::mem::{offset_of, size_of};

/// A resource limit: `struct rlimit`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct rlimit {
    /// The limit in force.
    pub rlim_cur: u64,
    /// How far the limit may be raised.
    pub rlim_max: u64,
}

/// A submission queue entry: `struct io_uring_sqe`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_sqe {
    /// The operation, an `IORING_OP_*`.
    pub opcode: u8,
    /// `IOSQE_*` flags.
    pub flags: u8,
    /// The priority, or the flags of a send, receive or accept.
    pub ioprio: u16,
    /// The descriptor, or an index under
    /// [`IOSQE_FIXED_FILE`](super::IOSQE_FIXED_FILE).
    pub fd: i32,
    /// The file offset, a second address, or a command, by operation.
    pub off: u64,
    /// The address of the buffer, path or other memory.
    pub addr: u64,
    /// The length in bytes, or a count.
    pub len: u32,
    /// The operation's own flags.
    pub op_flags: u32,
    /// Returned unchanged in the completion.
    pub user_data: u64,
    /// A registered buffer's index, or a provided buffer group's id.
    pub buf_index: u16,
    /// The registered credentials to run under; 0 for the task's own.
    pub personality: u16,
    /// The slot a new file takes in the registered file table, counted from 1;
    /// 0 for an ordinary descriptor.
    pub file_index: u32,
    /// A third address, or the address of a socket option's value, by
    /// operation.
    pub addr3: u64,
    /// Not used by a 64-byte entry.
    pub pad2: u64,
}

/// A completion queue entry: `struct io_uring_cqe`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct io_uring_cqe {
    /// The submission's `user_data`.
    pub user_data: u64,
    /// The result, or a negated errno.
    pub res: i32,
    /// `IORING_CQE_F_*` flags.
    pub flags: u32,
}

/// Where the submission ring's parts are: `struct io_sqring_offsets`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_sqring_offsets {
    /// Offset of the head, which the kernel advances.
    pub head: u32,
    /// Offset of the tail, which the program advances.
    pub tail: u32,
    /// Offset of the mask that turns a position into an index.
    pub ring_mask: u32,
    /// Offset of the number of entries.
    pub ring_entries: u32,
    /// Offset of the ring's flags.
    pub flags: u32,
    /// Offset of the count of invalid entries dropped.
    pub dropped: u32,
    /// Offset of the array of entry indices.
    pub array: u32,
    /// Reserved.
    pub resv1: u32,
    /// Address of the ring in memory the program gave, if it gave any.
    pub user_addr: u64,
}

/// Where the completion ring's parts are: `struct io_cqring_offsets`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_cqring_offsets {
    /// Offset of the head, which the program advances.
    pub head: u32,
    /// Offset of the tail, which the kernel advances.
    pub tail: u32,
    /// Offset of the mask that turns a position into an index.
    pub ring_mask: u32,
    /// Offset of the number of entries.
    pub ring_entries: u32,
    /// Offset of the count of completions that did not fit.
    pub overflow: u32,
    /// Offset of the entries.
    pub cqes: u32,
    /// Offset of the ring's flags.
    pub flags: u32,
    /// Reserved.
    pub resv1: u32,
    /// Address of the ring in memory the program gave, if it gave any.
    pub user_addr: u64,
}

/// What `io_uring_setup` takes and gives back: `struct io_uring_params`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_params {
    /// Submission queue entries, filled in by the kernel.
    pub sq_entries: u32,
    /// Completion queue entries: asked for with
    /// [`IORING_SETUP_CQSIZE`](super::IORING_SETUP_CQSIZE), filled in by the
    /// kernel.
    pub cq_entries: u32,
    /// `IORING_SETUP_*` flags.
    pub flags: u32,
    /// The processor the kernel's submission thread runs on.
    pub sq_thread_cpu: u32,
    /// Milliseconds before the kernel's submission thread sleeps.
    pub sq_thread_idle: u32,
    /// `IORING_FEAT_*` flags, filled in by the kernel.
    pub features: u32,
    /// A ring to share worker threads with.
    pub wq_fd: u32,
    /// Reserved.
    pub resv: [u32; 3],
    /// Where the submission ring's parts are, filled in by the kernel.
    pub sq_off: io_sqring_offsets,
    /// Where the completion ring's parts are, filled in by the kernel.
    pub cq_off: io_cqring_offsets,
}

/// A run of memory: `struct iovec`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct iovec {
    /// Where it starts.
    pub iov_base: *mut c_void,
    /// Its length in bytes.
    pub iov_len: usize,
}

/// Entries to replace in the registered file table: `struct
/// io_uring_files_update`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_files_update {
    /// The first entry to replace.
    pub offset: u32,
    /// Reserved.
    pub resv: u32,
    /// Address of the new descriptors.
    pub fds: u64,
}

/// The slots the kernel may allocate from: `struct io_uring_file_index_range`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_file_index_range {
    /// The first slot.
    pub off: u32,
    /// How many slots.
    pub len: u32,
    /// Reserved.
    pub resv: u64,
}

/// A ring of provided buffers to register: `struct io_uring_buf_reg`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_buf_reg {
    /// Address of the ring.
    pub ring_addr: u64,
    /// How many entries the ring has, a power of two.
    pub ring_entries: u32,
    /// The buffer group's id.
    pub bgid: u16,
    /// Padding on Linux 6.1; flags on later kernels. Zero on both.
    pub pad: u16,
    /// Reserved.
    pub resv: [u64; 3],
}

/// An entry in a ring of provided buffers: `struct io_uring_buf`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_buf {
    /// Address of the buffer.
    pub addr: u64,
    /// Its length in bytes.
    pub len: u32,
    /// The buffer's id, reported in the completion that uses it.
    pub bid: u16,
    /// `io_uring_buf_ring.tail` aliases `bufs[0].resv` at byte offset 14.
    pub resv: u16,
}

/// `struct __kernel_timespec`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct timespec {
    /// Seconds.
    pub sec: i64,
    /// Nanoseconds.
    pub nsec: i64,
}

const _: () = {
    assert!(size_of::<io_uring_sqe>() == 64);
    assert!(size_of::<io_uring_cqe>() == 16);
    assert!(size_of::<io_uring_params>() == 120);
    assert!(size_of::<io_uring_file_index_range>() == 16);
    assert!(size_of::<io_uring_buf_reg>() == 40);
    assert!(size_of::<io_uring_buf>() == 16);
    assert!(offset_of!(io_uring_buf, resv) == 14);
};
