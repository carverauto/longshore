// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! [`UringIo`] initialization: ring flags, `getrlimit` checks, and `<= 1 GiB` range registration.

use super::*;

const ONE_GIB: usize = 1 << 30;

/// Registered buffers are locked memory. Where the limit on that is too low
/// the kernel answers ENOMEM, which reads as if the machine were out of
/// memory, so name the limit and the pool size.
pub(super) fn name_memlock_limit(e: io::Error, pool_bytes: usize) -> io::Error {
    if e.kind() != io::ErrorKind::OutOfMemory {
        return e;
    }
    let mut lim = rlimit::default();
    let limit_desc = if unsafe { getrlimit(RLIMIT_MEMLOCK, &mut lim) } == 0 {
        if lim.rlim_cur == RLIM_INFINITY {
            "unlimited".to_string()
        } else {
            format!("{} bytes", lim.rlim_cur)
        }
    } else {
        "unknown".to_string()
    };
    io::Error::new(
        e.kind(),
        format!(
            "{e}: the locked-memory limit (RLIMIT_MEMLOCK = {limit_desc}, shared across all \
             rings of this user) is too low for the registered buffer pool ({pool_bytes} bytes)"
        ),
    )
}

fn check_nofile_limit(limits: &Limits) -> io::Result<u64> {
    let table_len = u64::from(limits.sockets_max) + u64::from(limits.files_max);
    let mut nofile = rlimit::default();
    if unsafe { getrlimit(RLIMIT_NOFILE, &mut nofile) } == 0 && table_len > nofile.rlim_cur {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "registered file table size {table_len} (sockets_max {} + files_max {}) \
                 exceeds RLIMIT_NOFILE ({})",
                limits.sockets_max, limits.files_max, nofile.rlim_cur
            ),
        ));
    }
    Ok(table_len)
}

fn register_pool_ranges(
    ring: &mut Ring,
    limits: &Limits,
    bufs: &mut [u8],
    pool_base: usize,
) -> io::Result<Box<[u16]>> {
    let count = limits.buffer_count() as usize;
    let mut buf_range = vec![0u16; count].into_boxed_slice();
    if count == 0 {
        return Ok(buf_range);
    }
    let base_ptr = unsafe { bufs.as_mut_ptr().add(pool_base) };
    let mut iov: Vec<iovec> = Vec::new();
    let (mut cur_start, mut cur_len) = (0usize, 0usize);
    for id in 0..count {
        let (off, len) = limits.span(BufId(id as u32));
        if len > ONE_GIB {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("buffer {id} length {len} exceeds 1 GiB registered range limit"),
            ));
        }
        if len == 0 {
            continue;
        }
        if cur_len == 0 {
            cur_start = off;
            cur_len = len;
        } else if off == cur_start + cur_len && cur_len + len <= ONE_GIB {
            cur_len += len;
        } else {
            iov.push(iovec {
                iov_base: unsafe { base_ptr.add(cur_start) }.cast(),
                iov_len: cur_len,
            });
            cur_start = off;
            cur_len = len;
        }
        buf_range[id] = iov.len() as u16;
    }
    if cur_len > 0 {
        iov.push(iovec {
            iov_base: unsafe { base_ptr.add(cur_start) }.cast(),
            iov_len: cur_len,
        });
    }
    if !iov.is_empty() {
        ring.register_buffers(&iov)
            .map_err(|e| name_memlock_limit(e, limits.pool_bytes()))?;
    }
    Ok(buf_range)
}

pub(super) fn cq_size(
    sq_entries: u32,
    ops_max: u32,
    ring_bufs: u32,
    accept_slots: u32,
) -> io::Result<u32> {
    let need = u64::from(ops_max) * 2 + u64::from(ring_bufs) + u64::from(accept_slots);
    let min = need.max(u64::from(sq_entries));
    u32::try_from(min)
        .ok()
        .and_then(|n| n.checked_next_power_of_two())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("completion queue size {min} exceeds u32 power-of-two limit"),
            )
        })
}

pub(super) fn check_features(features: u32) -> io::Result<()> {
    if features & IORING_FEAT_NODROP == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kernel io_uring lacks IORING_FEAT_NODROP",
        ));
    }
    Ok(())
}

impl UringIo {
    /// Allocates and registers everything. Call on the thread that will use it.
    pub fn new(limits: Limits) -> io::Result<UringIo> {
        limits.validate();
        let table_len = check_nofile_limit(&limits)?;
        // Room for one SQE + one internal cancel SQE per operation, and `quiesce`'s two SQEs.
        let entries = (limits.ops_max * 2 + 2).next_power_of_two().max(8);
        let cq_entries = cq_size(
            entries,
            limits.ops_max,
            limits.ring_buffers,
            limits.accept_range_len,
        )?;
        let mut ring = Ring::with_cq(
            entries,
            cq_entries,
            IORING_SETUP_SINGLE_ISSUER | IORING_SETUP_DEFER_TASKRUN | IORING_SETUP_SUBMIT_ALL,
        )?;
        check_features(ring.params.features)?;
        let mut bufs = vec![0u8; limits.pool_bytes()].into_boxed_slice();
        let pool_base = limits.pool_base(bufs.as_ptr() as usize);
        let buf_range = register_pool_ranges(&mut ring, &limits, &mut bufs, pool_base)?;
        if table_len > 0 {
            ring.register_files(&vec![-1i32; table_len as usize])?;
        }
        if limits.accept_range_len > 0 {
            ring.register_file_alloc_range(limits.accept_range_start, limits.accept_range_len)?;
        }
        let buf_ring = if limits.ring_buffers > 0 {
            Some(
                BufRing::new(&mut ring, &limits, &bufs, pool_base)
                    .map_err(|e| name_memlock_limit(e, limits.pool_bytes()))?,
            )
        } else {
            None
        };
        let ring_in_ring = vec![true; limits.ring_buffers as usize].into_boxed_slice();
        let slot = Slot {
            cancel_requested: false,
            token: 0,
            state: State::Free,
            result: 0,
            sock: 0,
            is_stream: false,
            file_slot: FileSlot(0),
            file_direct: false,
            op_sock: None,
            sock_closed: false,
            fd: -1,
            file_addr: 0,
            file_len: 0,
            phase: 0,
            addr: sockaddr_any::default(),
            addr_len: 0,
            probe_fd: -1,
            ts: timespec::default(),
            wake_count: 0,
        };
        Ok(UringIo {
            ring: ManuallyDrop::new(ring),
            limits,
            bufs,
            pool_base,
            buf_range,
            buf_ring,
            ring_in_ring,
            slots: vec![slot; limits.ops_max as usize].into_boxed_slice(),
            sock_stream: vec![false; limits.sockets_max as usize].into_boxed_slice(),
            in_flight: 0,
            quiet_ts: Box::new(timespec {
                sec: 0,
                nsec: 100_000_000,
            }),
            free: (0..limits.ops_max).rev().collect(),
            ready: ReadyQueue::new(limits.ops_max as usize),
            files_table: FileTable::new(limits.files_max),
            files: Vec::new(),
            wake_fd: -1,
            measure_segments: false,
            no_delay: false,
            keep_alive: false,
            last_listen_opts: None,
            last_connect_opts: None,
            #[cfg(feature = "fault-injection")]
            fail_tick: false,
        })
    }
}

#[cfg(test)]
mod setup_tests {
    use super::{
        check_features, cq_size, name_memlock_limit, IORING_FEAT_NODROP, IORING_FEAT_SINGLE_MMAP,
    };
    use std::io;

    const ENOMEM: i32 = 12;
    const EINVAL: i32 = 22;

    fn as_ring_reports(errno: i32) -> io::Error {
        let e = io::Error::from_raw_os_error(errno);
        io::Error::new(
            e.kind(),
            format!("io_uring_register(op=0, n=4) failed: {e}"),
        )
    }

    #[test]
    fn enomem_from_registration_names_the_limit_and_keeps_the_call() {
        let e = name_memlock_limit(as_ring_reports(ENOMEM), 65536);
        assert_eq!(e.kind(), io::ErrorKind::OutOfMemory);
        let text = e.to_string();
        assert!(text.contains("RLIMIT_MEMLOCK"), "{text}");
        assert!(text.contains("65536 bytes"), "{text}");
        assert!(text.contains("shared across all rings"), "{text}");
        assert!(text.contains("io_uring_register(op=0, n=4)"), "{text}");
    }

    #[test]
    fn other_errors_pass_through_unchanged() {
        let before = as_ring_reports(EINVAL).to_string();
        let e = name_memlock_limit(as_ring_reports(EINVAL), 65536);
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(e.to_string(), before);
    }

    #[test]
    fn cq_size_covers_ops_ring_bufs_accept_slots_and_sq_floor() {
        assert_eq!(cq_size(16, 4, 0, 0).unwrap(), 16);
        assert_eq!(cq_size(16, 10, 0, 0).unwrap(), 32);
        assert_eq!(cq_size(16, 8, 16, 8).unwrap(), 64);
        assert_eq!(cq_size(16, 8, 1000, 1000).unwrap(), 2048);
        assert_eq!(
            cq_size(16, u32::MAX, 0, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn check_features_requires_nodrop() {
        assert!(check_features(IORING_FEAT_NODROP | IORING_FEAT_SINGLE_MMAP).is_ok());
        let err = check_features(IORING_FEAT_SINGLE_MMAP).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("IORING_FEAT_NODROP"));
    }
}
