// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-01`–`T3-02`: `Engine<Posix>` file + TCP ops
//! in one tick loop, and `Limits::BASE` + aligned pool + `submit_chain`.

use super::super::harness::{
    drive_until, read_pool_bytes, submit_and_wait_one, write_pool_bytes, TempDir,
};
use super::{disk_config, file_limits, open_file, write_path};
use longshore::io::files::file_size;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token};
use longshore::sim::{Faults, SimIo};

fn run_t3_02_backend<IO: Io>(io: &mut IO, path_a: &str) {
    let mut out = Completions::new(16);
    let aligned_buf = BufId(8);
    let read_buf = BufId(9);
    let path_buf = BufId(4);
    let ptr = io.buf(aligned_buf).as_ptr() as usize;
    assert_eq!(ptr % 4096, 0, "aligned buffer must start on block boundary");

    let open_res = open_file(
        io,
        &mut out,
        Token(1),
        FileSlot(0),
        path_buf,
        path_a,
        OpenFlags {
            read: true,
            write: true,
            create: true,
            direct: true,
            ..Default::default()
        },
    )
    .expect("open direct");
    assert_eq!(open_res, 0);

    let pattern: Vec<u8> = (0..4096).map(|i| ((i * 7 + 3) & 0xff) as u8).collect();
    write_pool_bytes(io, aligned_buf, 0, &pattern);
    let chain = [
        (
            Token(2),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: aligned_buf,
                off: 0,
                len: 4096,
                file_off: 0,
            },
        ),
        (
            Token(3),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ];
    io.submit_chain(&chain).expect("submit_chain");
    let cs = drive_until(io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(2)) && v.iter().any(|c| c.token == Token(3))
    })
    .expect("chain drive");
    assert_eq!(
        cs.iter().find(|c| c.token == Token(2)).unwrap().result,
        4096
    );
    assert_eq!(cs.iter().find(|c| c.token == Token(3)).unwrap().result, 0);

    let r = submit_and_wait_one(
        io,
        &mut out,
        Token(4),
        Op::FileRead {
            slot: FileSlot(0),
            buf: read_buf,
            off: 0,
            len: 4096,
            file_off: 0,
        },
        10_000,
    )
    .expect("read");
    assert_eq!(r.result, 4096);
    assert_eq!(read_pool_bytes(io, read_buf, 0, 4096), pattern);

    let c = submit_and_wait_one(
        io,
        &mut out,
        Token(5),
        Op::FileClose { slot: FileSlot(0) },
        10_000,
    )
    .expect("close");
    assert_eq!(c.result, 0);
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
        OpenFlags {
            read: true,
            write: true,
            create: true,
            ..Default::default()
        },
    )
    .expect("open");
    assert_eq!(open_rc, 0);

    let payload = b"t3-01-posix-file-and-tcp";
    write_pool_bytes(&mut io, BufId(0), 0, payload);
    write_pool_bytes(&mut io, BufId(1), 0, b"ping-tcp");

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
        Op::FileWrite {
            slot: FileSlot(0),
            buf: BufId(0),
            off: 0,
            len: payload.len() as u32,
            file_off: 0,
        },
    )
    .unwrap();
    let cs = drive_until(&mut io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(10))
            && v.iter().any(|c| c.token == Token(11))
            && v.iter().any(|c| c.token == Token(12))
    })
    .unwrap();
    assert_eq!(
        cs.iter().find(|c| c.token == Token(12)).unwrap().result,
        payload.len() as i32
    );

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
    io.submit(
        Token(15),
        Op::FileSync {
            slot: FileSlot(0),
            datasync: false,
        },
    )
    .unwrap();
    let cs2 = drive_until(&mut io, &mut out, 10_000, |v| {
        v.iter().any(|c| c.token == Token(14)) && v.iter().any(|c| c.token == Token(15))
    })
    .unwrap();
    assert_eq!(cs2.iter().find(|c| c.token == Token(15)).unwrap().result, 0);
    assert_eq!(read_pool_bytes(&mut io, BufId(2), 0, 8), b"ping-tcp");

    assert_eq!(
        submit_and_wait_one(
            &mut io,
            &mut out,
            Token(16),
            Op::FileClose { slot: FileSlot(0) },
            10_000,
        )
        .unwrap()
        .result,
        0
    );
    let old_len = write_path(&mut io, BufId(4), path_a.to_str().unwrap());
    let new_len = write_path(&mut io, BufId(5), path_b.to_str().unwrap());
    let ren = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(17),
        Op::FileRename {
            old_buf: BufId(4),
            old_off: 0,
            old_len,
            new_buf: BufId(5),
            new_off: 0,
            new_len,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(ren.result, 0);

    let st_len = write_path(&mut io, BufId(4), path_b.to_str().unwrap());
    let st = submit_and_wait_one(
        &mut io,
        &mut out,
        Token(18),
        Op::FileStatus {
            path_buf: BufId(4),
            path_off: 0,
            path_len: st_len,
            out_buf: BufId(3),
            out_off: 0,
        },
        10_000,
    )
    .unwrap();
    assert_eq!(st.result, 0);
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
