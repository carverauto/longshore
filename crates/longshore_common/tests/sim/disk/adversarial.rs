// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Adversarial Tier 5 tests for `SimIo` UDP `RecvNow` and `SimDisk` edge cases.

use super::{
    drive, make_sim, open_path, put_path, read_at, rw_create, rw_open, stat_size, sync_dir_path,
    sync_slot, write_fill,
};
use longshore_common::io::posix::errno::linux::{EAGAIN, ENOENT, ENOTEMPTY};
use longshore_common::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock};
use longshore_common::sim::{Faults, PlacedFault, SimIo};

pub(super) fn mkdir_path(sim: &mut SimIo, out: &mut Completions, tok: u64, path: &str) -> i32 {
    let len = put_path(sim, BufId(0), path);
    drive(
        sim,
        out,
        tok,
        Op::FileMkdir {
            path_buf: BufId(0),
            path_off: 0,
            path_len: len,
        },
    )
}

pub(super) fn rename_path(
    sim: &mut SimIo,
    out: &mut Completions,
    tok: u64,
    old: &str,
    new: &str,
) -> i32 {
    let ol = put_path(sim, BufId(0), old);
    let nl = put_path(sim, BufId(1), new);
    drive(
        sim,
        out,
        tok,
        Op::FileRename {
            old_buf: BufId(0),
            old_off: 0,
            old_len: ol,
            new_buf: BufId(1),
            new_off: 0,
            new_len: nl,
        },
    )
}

fn recv_now(sim: &mut SimIo, out: &mut Completions, tok: u64, sock: Sock) -> i32 {
    drive(
        sim,
        out,
        tok,
        Op::RecvNow {
            sock,
            buf: BufId(1),
            off: 0,
            len: 64,
        },
    )
}

fn send_msg(sim: &mut SimIo, out: &mut Completions, tok: u64, sock: Sock, msg: &[u8]) -> i32 {
    sim.buf(BufId(0))[..msg.len()].copy_from_slice(msg);
    drive(
        sim,
        out,
        tok,
        Op::Send {
            sock,
            buf: BufId(0),
            off: 0,
            len: msg.len() as u32,
            zero_copy: false,
        },
    )
}

fn udp_sim() -> SimIo {
    SimIo::new(
        Limits {
            sockets_max: 4,
            buffers: 2,
            buffer_size: 256,
            ops_max: 8,
            ..Limits::BASE
        },
        11,
        Faults::default(),
    )
}

#[test]
fn sim_udp_recv_now_returns_eagain_when_empty_and_dgram_when_ready() {
    let mut sim = udp_sim();
    let mut out = Completions::new(8);
    let srv_addr = "192.0.2.53:5353".parse().unwrap();
    sim.bind_udp(Sock(0), srv_addr);
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            1,
            Op::ConnectUdp {
                sock: Sock(1),
                addr: srv_addr,
            },
        ),
        0
    );

    // Empty UDP bound and connected sockets both return -EAGAIN on RecvNow.
    assert_eq!(recv_now(&mut sim, &mut out, 2, Sock(0)), -EAGAIN);
    assert_eq!(recv_now(&mut sim, &mut out, 3, Sock(1)), -EAGAIN);

    // Send datagram client -> server, read with RecvNow, reply server -> client, read with RecvNow.
    assert_eq!(send_msg(&mut sim, &mut out, 4, Sock(1), b"query"), 5);
    assert_eq!(recv_now(&mut sim, &mut out, 5, Sock(0)), 5);
    assert_eq!(&sim.buf(BufId(1))[..5], b"query");

    assert_eq!(send_msg(&mut sim, &mut out, 6, Sock(0), b"reply"), 5);
    assert_eq!(recv_now(&mut sim, &mut out, 7, Sock(1)), 5);
    assert_eq!(&sim.buf(BufId(1))[..5], b"reply");

    // Default OpenFlags (directory: true, create: true) on missing dir -> -ENOENT.
    let mut dsim = make_sim(12, 512, 4, 1024);
    assert_eq!(
        open_path(
            &mut dsim,
            &mut out,
            8,
            FileSlot(0),
            "/missing_dir",
            OpenFlags {
                directory: true,
                create: true,
                ..Default::default()
            },
        ),
        -ENOENT
    );
}

#[test]
fn simdisk_corrupt_medium_bit_on_unsynced_reserve_and_placed_short_write_subsector() {
    let mut sim = make_sim(31, 512, 8, 4096);
    let mut out = Completions::new(16);

    assert_eq!(
        open_path(
            &mut sim,
            &mut out,
            1,
            FileSlot(0),
            "/sparse.dat",
            rw_create()
        ),
        0
    );
    assert_eq!(
        drive(
            &mut sim,
            &mut out,
            2,
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 4096,
                keep_size: false,
            },
        ),
        0
    );

    // BUG-04: corrupt_medium_bit on unsynced FileReserve hole flips the medium byte.
    sim.corrupt_medium_bit("/sparse.dat", 100, 0).unwrap();
    assert_eq!(read_at(&mut sim, &mut out, 3, FileSlot(0), 100, 1), 1);
    assert_eq!(sim.buf(BufId(2))[0], 0x01);
    assert_eq!(stat_size(&mut sim, &mut out, 4, "/sparse.dat"), 4096);

    // Sync persists the corrupted byte across power_loss.
    assert_eq!(sync_slot(&mut sim, &mut out, 5, FileSlot(0)), 0);
    sync_dir_path(&mut sim, &mut out, 6, "/");
    sim.power_loss();
    assert_eq!(
        open_path(&mut sim, &mut out, 9, FileSlot(0), "/sparse.dat", rw_open()),
        0
    );
    assert_eq!(read_at(&mut sim, &mut out, 10, FileSlot(0), 100, 1), 1);
    assert_eq!(sim.buf(BufId(2))[0], 0x01);

    // BUG-05: PlacedFault::ShortWrite on sub-sector write (256B < 512B sector) does not return 0.
    let mut sim2 = make_sim(32, 512, 8, 4096);
    assert_eq!(
        open_path(&mut sim2, &mut out, 1, FileSlot(0), "/sub.dat", rw_create()),
        0
    );
    sim2.place_disk_fault(2, PlacedFault::ShortWrite);
    assert_eq!(
        write_fill(&mut sim2, &mut out, 2, FileSlot(0), 0, 256, 0xAB),
        256
    );
    assert_eq!(read_at(&mut sim2, &mut out, 3, FileSlot(0), 0, 256), 256);
    assert!(sim2.buf(BufId(2))[..256].iter().all(|&b| b == 0xAB));
}

#[test]
fn simdisk_zero_write_log_writes_direct_and_rename_over_empty_or_nonempty_dir() {
    // GAP-S1: write_log_entries = 0, write_log_bytes = 0 -> immediate durable writes.
    let mut sim = make_sim(41, 512, 0, 0);
    let mut out = Completions::new(16);
    assert_eq!(
        open_path(&mut sim, &mut out, 1, FileSlot(0), "/wt.dat", rw_create()),
        0
    );
    sync_dir_path(&mut sim, &mut out, 2, "/");
    assert_eq!(
        write_fill(&mut sim, &mut out, 5, FileSlot(0), 0, 128, 0x5E),
        128
    );
    sim.power_loss();
    assert_eq!(
        open_path(&mut sim, &mut out, 6, FileSlot(0), "/wt.dat", rw_open()),
        0
    );
    assert_eq!(read_at(&mut sim, &mut out, 7, FileSlot(0), 0, 128), 128);
    assert!(sim.buf(BufId(2))[..128].iter().all(|&b| b == 0x5E));

    // GAP-S2: rename("/src", "/empty_dst") -> 0; rename("/src2", "/nonempty_dst") -> -ENOTEMPTY.
    let mut dsim = make_sim(42, 512, 8, 4096);
    assert_eq!(mkdir_path(&mut dsim, &mut out, 10, "/src"), 0);
    assert_eq!(mkdir_path(&mut dsim, &mut out, 11, "/empty_dst"), 0);
    assert_eq!(
        open_path(&mut dsim, &mut out, 12, FileSlot(0), "/src/a", rw_create()),
        0
    );
    assert_eq!(
        drive(&mut dsim, &mut out, 13, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(
        rename_path(&mut dsim, &mut out, 14, "/src", "/empty_dst"),
        0
    );
    sync_dir_path(&mut dsim, &mut out, 15, "/empty_dst");
    sync_dir_path(&mut dsim, &mut out, 18, "/");
    dsim.power_loss();
    assert_eq!(stat_size(&mut dsim, &mut out, 21, "/empty_dst/a"), 0);

    assert_eq!(mkdir_path(&mut dsim, &mut out, 21, "/src2"), 0);
    assert_eq!(mkdir_path(&mut dsim, &mut out, 22, "/nonempty"), 0);
    assert_eq!(
        open_path(
            &mut dsim,
            &mut out,
            23,
            FileSlot(0),
            "/nonempty/keep",
            rw_create(),
        ),
        0
    );
    assert_eq!(
        drive(&mut dsim, &mut out, 24, Op::FileClose { slot: FileSlot(0) }),
        0
    );
    assert_eq!(
        rename_path(&mut dsim, &mut out, 25, "/src2", "/nonempty"),
        -ENOTEMPTY
    );
}
