// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-01`–`T3-02`: `Engine<Posix>` file + TCP ops
//! in one tick loop, and `Limits::BASE` + aligned pool + `submit_chain`.

use super::super::harness::{drive_until, read_pool_bytes, write_pool_bytes, TempDir};
use super::{
    disk_config, file_limits, open_file, read_op, result_of, run_op, rw_create, sync_op, write_op,
    write_path,
};
use longshore::io::files::file_size;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token};
use longshore::sim::{Faults, SimIo};
#[cfg(unix)]
use std::net::SocketAddr;

fn run_t3_02_backend<IO: Io>(io: &mut IO, path_a: &str) {
    let mut out = Completions::new(16);
    let aligned_buf = BufId(8);
    let read_buf = BufId(9);
    let path_buf = BufId(4);
    let ptr = io.buf(aligned_buf).as_ptr() as usize;
    assert_eq!(ptr % 4096, 0, "aligned buffer must start on block boundary");

    let direct = OpenFlags {
        direct: true,
        ..rw_create()
    };
    let open_res = open_file(
        io,
        &mut out,
        Token(1),
        FileSlot(0),
        path_buf,
        path_a,
        direct,
    )
    .expect("open direct");
    assert_eq!(open_res, 0);

    let pattern: Vec<u8> = (0..4096u32).map(|i| ((i * 7 + 3) & 0xff) as u8).collect();
    write_pool_bytes(io, aligned_buf, 0, &pattern);
    let chain = [
        (Token(2), write_op(FileSlot(0), aligned_buf, 4096, 0)),
        (Token(3), sync_op(FileSlot(0), true)),
    ];
    io.submit_chain(&chain).expect("submit_chain");
    let cs = drive_until(io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(2)) && v.iter().any(|c| c.token == Token(3))
    })
    .expect("chain drive");
    assert_eq!(result_of(&cs, 2), 4096);
    assert_eq!(result_of(&cs, 3), 0);

    let r = run_op(
        io,
        &mut out,
        4,
        read_op(FileSlot(0), read_buf, 4096, 0),
        10_000,
    );
    assert_eq!(r, 4096);
    assert_eq!(read_pool_bytes(io, read_buf, 0, 4096), pattern);

    let c = run_op(io, &mut out, 5, Op::FileClose { slot: FileSlot(0) }, 10_000);
    assert_eq!(c, 0);
}

/// Accept, connect and a file write in one tick loop; the write must land whole.
#[cfg(unix)]
fn connect_while_writing<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    addr: SocketAddr,
    payload: &[u8],
) {
    io.submit(
        Token(10),
        Op::Accept {
            listener: Sock(0),
            sock: Sock(2),
        },
    )
    .unwrap();
    io.submit(
        Token(11),
        Op::Connect {
            sock: Sock(1),
            addr,
        },
    )
    .unwrap();
    io.submit(
        Token(12),
        write_op(FileSlot(0), BufId(0), payload.len() as u32, 0),
    )
    .unwrap();
    let cs = drive_until(io, out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(10))
            && v.iter().any(|c| c.token == Token(11))
            && v.iter().any(|c| c.token == Token(12))
    })
    .unwrap();
    assert_eq!(result_of(&cs, 12), i32::try_from(payload.len()).unwrap());
}

/// A TCP send/recv and a file sync in one tick loop.
#[cfg(unix)]
fn exchange_while_syncing<IO: Io>(io: &mut IO, out: &mut Completions) {
    io.submit(
        Token(13),
        Op::Send {
            sock: Sock(1),
            buf: BufId(1),
            off: 0,
            len: 8,
            zero_copy: false,
        },
    )
    .unwrap();
    io.submit(
        Token(14),
        Op::Recv {
            sock: Sock(2),
            buf: BufId(2),
            off: 0,
            len: 16,
        },
    )
    .unwrap();
    io.submit(Token(15), sync_op(FileSlot(0), false)).unwrap();
    let cs2 = drive_until(io, out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(14)) && v.iter().any(|c| c.token == Token(15))
    })
    .unwrap();
    assert_eq!(result_of(&cs2, 15), 0);
    assert_eq!(read_pool_bytes(io, BufId(2), 0, 8), b"ping-tcp");
}

/// Closes the file, renames `path_a` to `path_b`, and stats `path_b` into
/// `BufId(3)`; each must succeed.
#[cfg(unix)]
fn close_rename_and_stat<IO: Io>(io: &mut IO, out: &mut Completions, path_a: &str, path_b: &str) {
    assert_eq!(
        run_op(io, out, 16, Op::FileClose { slot: FileSlot(0) }, 10_000),
        0
    );
    let old_len = write_path(io, BufId(4), path_a);
    let new_len = write_path(io, BufId(5), path_b);
    let rename = Op::FileRename {
        old_buf: BufId(4),
        old_off: 0,
        old_len,
        new_buf: BufId(5),
        new_off: 0,
        new_len,
    };
    assert_eq!(run_op(io, out, 17, rename, 10_000), 0);

    let st_len = write_path(io, BufId(4), path_b);
    let status = Op::FileStatus {
        path_buf: BufId(4),
        path_off: 0,
        path_len: st_len,
        out_buf: BufId(3),
        out_off: 0,
    };
    assert_eq!(run_op(io, out, 18, status, 10_000), 0);
}

#[cfg(unix)]
#[test]
fn t3_01_engine_posix_file_ops_and_tcp_in_same_tick() {
    use super::super::harness::posix_backend;

    let dir = TempDir::new("t3_01").expect("tempdir");
    let path_a = dir.file_path("alpha.dat");
    let path_b = dir.file_path("beta.dat");
    let limits = file_limits(4, 2, 4096);
    let mut io = posix_backend(limits).expect("posix");
    let mut out = Completions::new(16);

    let addr = io
        .listen(Sock(0), "127.0.0.1:0".parse().unwrap())
        .expect("listen");
    let open_rc = open_file(
        &mut io,
        &mut out,
        Token(1),
        FileSlot(0),
        BufId(4),
        path_a.to_str().unwrap(),
        rw_create(),
    )
    .expect("open");
    assert_eq!(open_rc, 0);

    let payload = b"t3-01-posix-file-and-tcp";
    write_pool_bytes(&mut io, BufId(0), 0, payload);
    write_pool_bytes(&mut io, BufId(1), 0, b"ping-tcp");
    connect_while_writing(&mut io, &mut out, addr, payload);
    exchange_while_syncing(&mut io, &mut out);
    close_rename_and_stat(
        &mut io,
        &mut out,
        path_a.to_str().unwrap(),
        path_b.to_str().unwrap(),
    );
    assert_eq!(file_size(io.buf(BufId(3))), payload.len() as u64);
    assert_eq!(std::fs::read(&path_b).unwrap(), payload);
    assert!(!path_a.exists());
}

#[test]
fn t3_02_limits_base_aligned_pool_and_submit_chain() {
    let limits = Limits {
        files_max: 4,
        aligned_buffers: 4,
        aligned_buffer_size: 4096,
        block_size: 4096,
        buffers: 4,
        buffer_size: 4096,
        small_buffers: 4,
        small_buffer_size: 512,
        ops_max: 8,
        sockets_max: 4,
        ..Limits::BASE
    };
    let mut sim = SimIo::new(limits, 302, Faults::default());
    sim.setup_disk(disk_config());
    run_t3_02_backend(&mut sim, "/t3_02_sim.dat");

    #[cfg(unix)]
    {
        let dir = TempDir::new("t3_02").unwrap();
        let mut posix = super::super::harness::posix_backend(limits).unwrap();
        run_t3_02_backend(&mut posix, dir.file_path("p.dat").to_str().unwrap());
    }
    #[cfg(target_os = "linux")]
    {
        let dir = TempDir::new("t3_02_uring").unwrap();
        let mut uring = longshore::io::uring::UringIo::new(limits).unwrap();
        run_t3_02_backend(&mut uring, dir.file_path("u.dat").to_str().unwrap());
    }
}
