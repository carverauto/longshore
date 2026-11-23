// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! `T4-03`: Cross-backend parity mini file-server (`SimIo` vs `Posix` vs `UringIo`).
//!
//! Exercises armed `Accept`, armed `Recv` + `give_back`, TCP + Unix listeners,
//! `FileOpen`, `FileReserve`, `submit_chain([FileWrite, FileSync])`, `FileStatus`
//! (`statx`), `FileRead`, and shutdown-before-close across all backends.

use super::super::harness::{
    drive_until, read_pool_into, submit_and_wait_one, write_nul_path, write_pool_bytes,
};
use longshore::io::files::file_size;
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token};
use std::net::SocketAddr;

const TCP_LISTENER: Sock = Sock(0);
const UNIX_LISTENER: Sock = Sock(1);
const TCP_CLIENT: Sock = Sock(2);
const UNIX_CLIENT: Sock = Sock(3);
const ACCEPT_START: u32 = 4;
const ACCEPT_LEN: u32 = 6;

pub fn parity_limits() -> Limits {
    Limits {
        sockets_max: 10,
        buffers: 4,
        buffer_size: 4096,
        small_buffers: 4,
        small_buffer_size: 256,
        accept_range_start: ACCEPT_START,
        accept_range_len: ACCEPT_LEN,
        ring_buffers: 8,
        ring_buffer_size: 512,
        files_max: 4,
        ops_max: 16,
        ..Limits::BASE
    }
}

pub fn run_mini_fileserver<IO: Io>(
    io: &mut IO,
    lim: Limits,
    tcp_addr: SocketAddr,
    unix_path: &str,
    store_file: &str,
) {
    let mut out = Completions::new(16);
    let bound_tcp = io.listen(TCP_LISTENER, tcp_addr).unwrap();
    io.listen_unix(UNIX_LISTENER, unix_path).unwrap();

    // Arm Accept on both TCP and Unix listeners.
    io.submit(
        Token(1),
        Op::ArmedAccept {
            listener: TCP_LISTENER,
        },
    )
    .unwrap();
    io.submit(
        Token(2),
        Op::ArmedAccept {
            listener: UNIX_LISTENER,
        },
    )
    .unwrap();

    // Connect TCP client & Unix client.
    let upath_buf = lim.small(0);
    let ulen = write_nul_path(io, upath_buf, 0, unix_path);
    io.submit(
        Token(3),
        Op::Connect {
            sock: TCP_CLIENT,
            addr: bound_tcp,
        },
    )
    .unwrap();
    io.submit(
        Token(4),
        Op::ConnectUnix {
            sock: UNIX_CLIENT,
            path_buf: upath_buf,
            path_off: 0,
            path_len: ulen,
        },
    )
    .unwrap();

    let mut tcp_srv: Option<Sock> = None;
    let mut unix_srv: Option<Sock> = None;
    let mut c_tcp = false;
    let mut c_unix = false;
    let cs = drive_until(io, &mut out, 1_000, |v| {
        v.iter().any(|c| c.token.0 == 1 && c.result >= 0)
            && v.iter().any(|c| c.token.0 == 2 && c.result >= 0)
            && v.iter().any(|c| c.token.0 == 3)
            && v.iter().any(|c| c.token.0 == 4)
    })
    .unwrap();
    for c in cs {
        match c.token.0 {
            1 => tcp_srv = Some(Sock(c.result as u32)),
            2 => unix_srv = Some(Sock(c.result as u32)),
            3 => {
                assert!(c.result >= 0);
                c_tcp = true;
            }
            4 => {
                assert_eq!(c.result, 0);
                c_unix = true;
            }
            _ => {}
        }
    }
    assert!(c_tcp && c_unix);
    let tcp_srv = tcp_srv.unwrap();
    let unix_srv = unix_srv.unwrap();

    // Arm Recv on accepted sockets.
    io.submit(Token(10), Op::ArmedRecv { sock: tcp_srv })
        .unwrap();
    io.submit(Token(11), Op::ArmedRecv { sock: unix_srv })
        .unwrap();

    // TCP client sends a 256B PUT payload; server writes+syncs to disk via submit_chain.
    let payload = [0x5Au8; 256];
    write_pool_bytes(io, BufId(0), 0, &payload);
    io.submit(
        Token(20),
        Op::Send {
            sock: TCP_CLIENT,
            buf: BufId(0),
            off: 0,
            len: 256,
            zero_copy: false,
        },
    )
    .unwrap();

    let mut collected = Vec::new();
    let mut sent_ok = false;
    while collected.len() < 256 || !sent_ok {
        let cs = drive_until(io, &mut out, 1_000, |v| !v.is_empty()).unwrap();
        for c in cs {
            if c.token.0 == 20 {
                assert_eq!(c.result, 256);
                sent_ok = true;
            } else if c.token.0 == 10 && c.result > 0 {
                let bid = c.buf.expect("armed recv buf");
                collected.extend_from_slice(&io.buf(bid)[..c.result as usize]);
                io.give_back(bid);
            }
        }
    }
    assert_eq!(collected, payload);

    let fpath_buf = lim.small(1);
    let flen = write_nul_path(io, fpath_buf, 0, store_file);
    assert_eq!(
        submit_and_wait_one(
            io,
            &mut out,
            Token(30),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: fpath_buf,
                path_off: 0,
                path_len: flen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    empty: true,
                    ..Default::default()
                },
            },
            1_000,
        )
        .unwrap()
        .result,
        0
    );
    assert_eq!(
        submit_and_wait_one(
            io,
            &mut out,
            Token(31),
            Op::FileReserve {
                slot: FileSlot(0),
                file_off: 0,
                len: 512,
                keep_size: false,
            },
            1_000,
        )
        .unwrap()
        .result,
        0
    );

    write_pool_bytes(io, BufId(1), 0, &collected);
    io.submit_chain(&[
        (
            Token(32),
            Op::FileWrite {
                slot: FileSlot(0),
                buf: BufId(1),
                off: 0,
                len: 256,
                file_off: 0,
            },
        ),
        (
            Token(33),
            Op::FileSync {
                slot: FileSlot(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    let mut w_ok = false;
    let mut s_ok = false;
    while !(w_ok && s_ok) {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if c.token.0 == 32 {
                assert_eq!(c.result, 256);
                w_ok = true;
            } else if c.token.0 == 33 {
                assert_eq!(c.result, 0);
                s_ok = true;
            }
        }
    }

    let stat_buf = lim.small(2);
    assert_eq!(
        submit_and_wait_one(
            io,
            &mut out,
            Token(34),
            Op::FileStatus {
                path_buf: fpath_buf,
                path_off: 0,
                path_len: flen,
                out_buf: stat_buf,
                out_off: 0,
            },
            1_000,
        )
        .unwrap()
        .result,
        0
    );
    assert!(file_size(io.buf(stat_buf)) >= 512);

    assert_eq!(
        submit_and_wait_one(
            io,
            &mut out,
            Token(35),
            Op::FileRead {
                slot: FileSlot(0),
                buf: BufId(2),
                off: 0,
                len: 256,
                file_off: 0,
            },
            1_000,
        )
        .unwrap()
        .result,
        256
    );
    let mut read_back = [0u8; 256];
    read_pool_into(io, BufId(2), 0, &mut read_back);
    assert_eq!(read_back, payload);

    // Reply to Unix client and close server socket -> Unix client sees 0 EOF.
    write_pool_bytes(io, BufId(3), 0, &read_back[..32]);
    assert_eq!(
        submit_and_wait_one(
            io,
            &mut out,
            Token(40),
            Op::Send {
                sock: unix_srv,
                buf: BufId(3),
                off: 0,
                len: 32,
                zero_copy: false,
            },
            1_000,
        )
        .unwrap()
        .result,
        32
    );
    let _ =
        submit_and_wait_one(io, &mut out, Token(41), Op::Close { sock: unix_srv }, 1_000).unwrap();

    let mut u_got = Vec::new();
    let mut saw_eof = false;
    while !saw_eof {
        let rc = submit_and_wait_one(
            io,
            &mut out,
            Token(42 + u_got.len() as u64),
            Op::Recv {
                sock: UNIX_CLIENT,
                buf: BufId(0),
                off: 0,
                len: 64,
            },
            1_000,
        )
        .unwrap();
        if rc.result == 0 {
            saw_eof = true;
        } else {
            assert!(rc.result > 0);
            u_got.extend_from_slice(&io.buf(BufId(0))[..rc.result as usize]);
        }
    }
    assert_eq!(u_got, &payload[..32]);

    for (idx, s) in [
        TCP_CLIENT,
        UNIX_CLIENT,
        tcp_srv,
        TCP_LISTENER,
        UNIX_LISTENER,
    ]
    .iter()
    .enumerate()
    {
        let _ = submit_and_wait_one(
            io,
            &mut out,
            Token(60 + idx as u64),
            Op::Close { sock: *s },
            1_000,
        )
        .unwrap();
    }
    let _ = submit_and_wait_one(
        io,
        &mut out,
        Token(70),
        Op::FileClose { slot: FileSlot(0) },
        1_000,
    )
    .unwrap();
}
