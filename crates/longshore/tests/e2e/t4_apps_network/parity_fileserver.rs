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
use longshore::io::{
    BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Sock, Token,
};
use std::net::SocketAddr;

const TCP_LISTENER: Sock = Sock::new(0);
const UNIX_LISTENER: Sock = Sock::new(1);
const TCP_CLIENT: Sock = Sock::new(2);
const UNIX_CLIENT: Sock = Sock::new(3);
const ACCEPT_START: u32 = 4;
const ACCEPT_LEN: u32 = 6;

pub fn parity_limits() -> Limits {
    Limits::BASE
        .with_sockets_max(10)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_small_buffers(4)
        .with_small_buffer_size(256)
        .with_accept_range_start(ACCEPT_START)
        .with_accept_range_len(ACCEPT_LEN)
        .with_ring_buffers(8)
        .with_ring_buffer_size(512)
        .with_files_max(4)
        .with_ops_max(16)
}

/// Submits `op` and waits up to 1000 ticks for its result.
fn wait_result<IO: Io>(io: &mut IO, out: &mut Completions, token: Token, op: Op) -> i32 {
    submit_and_wait_one(io, out, token, op, 1_000)
        .unwrap()
        .result()
}

/// Arms accept on both listeners and connects a TCP and a Unix client;
/// returns the accepted TCP and Unix server sockets.
fn accept_tcp_and_unix<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    lim: Limits,
    bound_tcp: SocketAddr,
    unix_path: &str,
) -> (Sock, Sock) {
    io.submit(
        Token::new(1),
        Op::ArmedAccept {
            listener: TCP_LISTENER,
        },
    )
    .unwrap();
    io.submit(
        Token::new(2),
        Op::ArmedAccept {
            listener: UNIX_LISTENER,
        },
    )
    .unwrap();

    let upath_buf = lim.small(0);
    let ulen = write_nul_path(io, upath_buf, 0, unix_path);
    io.submit(
        Token::new(3),
        Op::Connect {
            sock: TCP_CLIENT,
            addr: bound_tcp,
        },
    )
    .unwrap();
    io.submit(
        Token::new(4),
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
    let cs = drive_until(io, out, 1_000, |v| {
        v.iter().any(|c| c.token().get() == 1 && c.result() >= 0)
            && v.iter().any(|c| c.token().get() == 2 && c.result() >= 0)
            && v.iter().any(|c| c.token().get() == 3)
            && v.iter().any(|c| c.token().get() == 4)
    })
    .unwrap();
    for c in cs {
        match c.token().get() {
            1 => tcp_srv = Some(Sock::new(u32::try_from(c.result()).unwrap())),
            2 => unix_srv = Some(Sock::new(u32::try_from(c.result()).unwrap())),
            3 => {
                assert!(c.result() >= 0);
                c_tcp = true;
            }
            4 => {
                assert_eq!(c.result(), 0);
                c_unix = true;
            }
            _ => {}
        }
    }
    assert!(c_tcp && c_unix);
    (tcp_srv.unwrap(), unix_srv.unwrap())
}

/// The TCP client sends a 256B PUT payload; the server collects it through
/// its armed recv, giving each ring buffer back.
fn receive_put<IO: Io>(io: &mut IO, out: &mut Completions, payload: &[u8; 256]) -> Vec<u8> {
    write_pool_bytes(io, BufId::new(0), 0, payload);
    io.submit(
        Token::new(20),
        Op::Send {
            sock: TCP_CLIENT,
            buf: BufId::new(0),
            off: 0,
            len: 256,
            zero_copy: false,
        },
    )
    .unwrap();

    let mut collected = Vec::new();
    let mut sent_ok = false;
    while collected.len() < 256 || !sent_ok {
        let batch = drive_until(io, out, 1_000, |v| !v.is_empty()).unwrap();
        for c in batch {
            if c.token().get() == 20 {
                assert_eq!(c.result(), 256);
                sent_ok = true;
            } else if c.token().get() == 10 && c.result() > 0 {
                let bid = c.buf().expect("armed recv buf");
                collected.extend_from_slice(&io.buf(bid)[..usize::try_from(c.result()).unwrap()]);
                io.give_back(bid);
            }
        }
    }
    collected
}

/// Opens (emptying) the store file in `FileSlot(0)` and reserves 512 bytes;
/// returns the path buffer and length for later `FileStatus`.
fn open_and_reserve<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    lim: Limits,
    store_file: &str,
) -> (BufId, u32) {
    let fpath_buf = lim.small(1);
    let flen = write_nul_path(io, fpath_buf, 0, store_file);
    let open = Op::FileOpen {
        slot: FileSlot::new(0),
        path_buf: fpath_buf,
        path_off: 0,
        path_len: flen,
        flags: OpenFlags::NONE
            .with_read(true)
            .with_write(true)
            .with_create(true)
            .with_empty(true),
    };
    assert_eq!(wait_result(io, out, Token::new(30), open), 0);
    let reserve = Op::FileReserve {
        slot: FileSlot::new(0),
        file_off: 0,
        len: 512,
        keep_size: false,
    };
    assert_eq!(wait_result(io, out, Token::new(31), reserve), 0);
    (fpath_buf, flen)
}

/// Writes `data` (256 bytes) and datasyncs it via `submit_chain`.
fn write_and_sync<IO: Io>(io: &mut IO, out: &mut Completions, data: &[u8]) {
    write_pool_bytes(io, BufId::new(1), 0, data);
    io.submit_chain(&[
        (
            Token::new(32),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: BufId::new(1),
                off: 0,
                len: 256,
                file_off: 0,
            },
        ),
        (
            Token::new(33),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
    let mut w_ok = false;
    let mut s_ok = false;
    while !(w_ok && s_ok) {
        io.tick(true, out).unwrap();
        while let Some(c) = out.pop() {
            if c.token().get() == 32 {
                assert_eq!(c.result(), 256);
                w_ok = true;
            } else if c.token().get() == 33 {
                assert_eq!(c.result(), 0);
                s_ok = true;
            }
        }
    }
}

/// `FileStatus` shows the reservation; `FileRead` returns the stored bytes.
fn stat_and_read_back<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    lim: Limits,
    (fpath_buf, flen): (BufId, u32),
) -> [u8; 256] {
    let stat_buf = lim.small(2);
    let status = Op::FileStatus {
        path_buf: fpath_buf,
        path_off: 0,
        path_len: flen,
        out_buf: stat_buf,
        out_off: 0,
    };
    assert_eq!(wait_result(io, out, Token::new(34), status), 0);
    assert!(file_size(io.buf(stat_buf)) >= 512);

    let read = Op::FileRead {
        slot: FileSlot::new(0),
        buf: BufId::new(2),
        off: 0,
        len: 256,
        file_off: 0,
    };
    assert_eq!(wait_result(io, out, Token::new(35), read), 256);
    let mut read_back = [0u8; 256];
    read_pool_into(io, BufId::new(2), 0, &mut read_back);
    read_back
}

/// Replies 32 bytes to the Unix client and closes its server socket; the
/// client reads everything up to EOF, which is returned.
fn reply_and_close_unix<IO: Io>(
    io: &mut IO,
    out: &mut Completions,
    unix_srv: Sock,
    reply: &[u8],
) -> Vec<u8> {
    write_pool_bytes(io, BufId::new(3), 0, reply);
    let send = Op::Send {
        sock: unix_srv,
        buf: BufId::new(3),
        off: 0,
        len: 32,
        zero_copy: false,
    };
    assert_eq!(wait_result(io, out, Token::new(40), send), 32);
    let _: Completion =
        submit_and_wait_one(io, out, Token::new(41), Op::Close { sock: unix_srv }, 1_000).unwrap();

    let mut u_got = Vec::new();
    let mut saw_eof = false;
    while !saw_eof {
        let rc = submit_and_wait_one(
            io,
            out,
            Token::new(42 + u_got.len() as u64),
            Op::Recv {
                sock: UNIX_CLIENT,
                buf: BufId::new(0),
                off: 0,
                len: 64,
            },
            1_000,
        )
        .unwrap();
        if rc.result() == 0 {
            saw_eof = true;
        } else {
            assert!(rc.result() > 0);
            u_got
                .extend_from_slice(&io.buf(BufId::new(0))[..usize::try_from(rc.result()).unwrap()]);
        }
    }
    u_got
}

/// Closes every remaining socket and the store file.
fn close_everything<IO: Io>(io: &mut IO, out: &mut Completions, tcp_srv: Sock) {
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
        let _: Completion = submit_and_wait_one(
            io,
            out,
            Token::new(60 + idx as u64),
            Op::Close { sock: *s },
            1_000,
        )
        .unwrap();
    }
    let _: Completion = submit_and_wait_one(
        io,
        out,
        Token::new(70),
        Op::FileClose {
            slot: FileSlot::new(0),
        },
        1_000,
    )
    .unwrap();
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
    let (tcp_srv, unix_srv) = accept_tcp_and_unix(io, &mut out, lim, bound_tcp, unix_path);

    // Arm Recv on accepted sockets.
    io.submit(Token::new(10), Op::ArmedRecv { sock: tcp_srv })
        .unwrap();
    io.submit(Token::new(11), Op::ArmedRecv { sock: unix_srv })
        .unwrap();

    // TCP client PUTs 256B; server writes+syncs it to disk via submit_chain.
    let payload = [0x5Au8; 256];
    let collected = receive_put(io, &mut out, &payload);
    assert_eq!(collected, payload);
    let path = open_and_reserve(io, &mut out, lim, store_file);
    write_and_sync(io, &mut out, &collected);
    let read_back = stat_and_read_back(io, &mut out, lim, path);
    assert_eq!(read_back, payload);

    // Reply to Unix client and close server socket -> Unix client sees 0 EOF.
    let u_got = reply_and_close_unix(io, &mut out, unix_srv, &read_back[..32]);
    assert_eq!(u_got, &payload[..32]);
    close_everything(io, &mut out, tcp_srv);
}
