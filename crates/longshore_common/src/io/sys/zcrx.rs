// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Kernel ABI of io_uring zero-copy receive and of the vectored write,
//! from the uapi header of Linux 7.0 (`include/uapi/linux/io_uring.h`).

use core::ffi::c_uint;

/// A ring whose completion entries are 32 bytes: a zero-copy receive puts
/// where its bytes lie into the second half.
pub const IORING_SETUP_CQE32: u32 = 1 << 11; // 5.19

/// Vectored write: `addr` an array of [`iovec`](super::iovec), `len` how many.
pub const IORING_OP_WRITEV: u8 = 2;
/// Multishot receive into a registered area, without a copy by the kernel.
pub const IORING_OP_RECV_ZC: u8 = 58; // 6.15

/// Ask which operations the kernel has: [`io_uring_probe`].
pub const IORING_REGISTER_PROBE: c_uint = 8; // 5.6
/// Register a receive queue of an interface with an area:
/// [`io_uring_zcrx_ifq_reg`].
pub const IORING_REGISTER_ZCRX_IFQ: c_uint = 32; // 6.15
/// A control call on a registration: [`zcrx_ctrl`].
pub const IORING_REGISTER_ZCRX_CTRL: c_uint = 36; // 6.19

/// [`io_uring_region_desc::flags`]: the region is the caller's memory.
pub const IORING_MEM_REGION_TYPE_USER: u32 = 1;
/// The bits of a completion's offset from this one up are the area's id.
pub const IORING_ZCRX_AREA_SHIFT: u32 = 48;
/// [`zcrx_ctrl::op`]: take every entry of the ring of returns now.
pub const ZCRX_CTRL_FLUSH_RQ: u32 = 0;
/// [`io_uring_probe_op::flags`]: the kernel has the operation.
pub const IO_URING_OP_SUPPORTED: u16 = 1;

/// Where the ring of returns' parts are in its region:
/// `struct io_uring_zcrx_offsets`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_zcrx_offsets {
    /// The head, which the kernel advances.
    pub head: u32,
    /// The tail, which the program advances.
    pub tail: u32,
    /// The entries.
    pub rqes: u32,
    /// Zero.
    pub resv2: u32,
    /// Zero.
    pub resv: [u64; 2],
}

/// The memory a registration receives into: `struct io_uring_zcrx_area_reg`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_zcrx_area_reg {
    /// Where it starts, at a page boundary.
    pub addr: u64,
    /// Its length, whole pages.
    pub len: u64,
    /// Set by the kernel: goes into the top bits of every return.
    pub rq_area_token: u64,
    /// Zero for the program's own memory.
    pub flags: u32,
    /// Unused without a dmabuf.
    pub dmabuf_fd: u32,
    /// Zero.
    pub resv2: [u64; 2],
}

/// A region of memory handed to the ring: `struct io_uring_region_desc`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_region_desc {
    /// Where it starts.
    pub user_addr: u64,
    /// Its length.
    pub size: u64,
    /// [`IORING_MEM_REGION_TYPE_USER`].
    pub flags: u32,
    /// Zero.
    pub id: u32,
    /// Zero for the program's own memory.
    pub mmap_offset: u64,
    /// Zero.
    pub resv: [u64; 4],
}

/// The argument of [`IORING_REGISTER_ZCRX_IFQ`]:
/// `struct io_uring_zcrx_ifq_reg`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_zcrx_ifq_reg {
    /// The interface's index.
    pub if_idx: u32,
    /// The receive queue of it.
    pub if_rxq: u32,
    /// Entries of the ring of returns; at most 32,768.
    pub rq_entries: u32,
    /// Zero.
    pub flags: u32,
    /// The address of an [`io_uring_zcrx_area_reg`].
    pub area_ptr: u64,
    /// The address of an [`io_uring_region_desc`] for the ring of returns.
    pub region_ptr: u64,
    /// Set by the kernel.
    pub offsets: io_uring_zcrx_offsets,
    /// Set by the kernel: names the registration in a receive.
    pub zcrx_id: u32,
    /// Zero for buffers of a page.
    pub rx_buf_len: u32,
    /// Zero.
    pub resv: [u64; 3],
}

/// The argument of [`IORING_REGISTER_ZCRX_CTRL`]: `struct zcrx_ctrl`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct zcrx_ctrl {
    /// The registration.
    pub zcrx_id: u32,
    /// What to do: [`ZCRX_CTRL_FLUSH_RQ`].
    pub op: u32,
    /// Zero.
    pub resv: [u64; 2],
    /// The operation's own argument; zero for a flush.
    pub body: [u64; 6],
}

/// One operation in a probe: `struct io_uring_probe_op`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct io_uring_probe_op {
    /// The operation.
    pub op: u8,
    /// Zero.
    pub resv: u8,
    /// [`IO_URING_OP_SUPPORTED`] when the kernel has it.
    pub flags: u16,
    /// Zero.
    pub resv2: u32,
}

/// The argument of [`IORING_REGISTER_PROBE`] with room for every operation
/// a `u8` can name: `struct io_uring_probe`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct io_uring_probe {
    /// The last operation the kernel knows.
    pub last_op: u8,
    /// How many of `ops` the kernel filled.
    pub ops_len: u8,
    /// Zero.
    pub resv: u16,
    /// Zero.
    pub resv2: [u32; 3],
    /// One entry an operation.
    pub ops: [io_uring_probe_op; 256],
}
