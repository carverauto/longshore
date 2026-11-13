// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scenarios that run unchanged on any `Io`.

use osc_io::{BufId, Completions, Io, Op, Sock, Token};
use std::net::SocketAddrV4;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EchoError {
    /// An operation failed: (token, negated errno).
    Io(u64, i32),
    /// Echoed byte at this stream offset differed from what was sent.
    Mismatch(u64),
    /// `Io::tick` or `Io::listen` itself failed.
    Driver,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct EchoStats {
    pub sends: u64,
    pub recvs: u64,
    pub ticks: u64,
}

const LISTENER: Sock = Sock(0);
const CLIENT: Sock = Sock(1);
const SERVER: Sock = Sock(2);
const CLIENT_TX: BufId = BufId(0);
const CLIENT_RX: BufId = BufId(1);
const SERVER_BUF: BufId = BufId(2);

const T_ACCEPT: u64 = 1;
const T_CONNECT: u64 = 2;
const T_CSEND: u64 = 3;
const T_CRECV: u64 = 4;
const T_SRECV: u64 = 5;
const T_SSEND: u64 = 6;
const T_CLOSE: u64 = 7;

/// Byte `i` of the stream the client sends.
fn pattern(i: u64) -> u8 {
    (i.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 56) as u8
}

/// A client sends `total` patterned bytes to an echo server on the same `Io`
/// and checks every byte that comes back. Needs 3 sockets, 3 buffers of at
/// least `chunk` bytes, and 4 operation slots. Performs no allocation.
pub fn echo<IO: Io>(
    io: &mut IO,
    addr: SocketAddrV4,
    total: u64,
    chunk: u32,
    zero_copy: bool,
    out: &mut Completions,
) -> Result<EchoStats, EchoError> {
    let addr = io.listen(LISTENER, addr).map_err(|_| EchoError::Driver)?;
    let sub = |io: &mut IO, t: u64, op: Op| {
        io.submit(Token(t), op)
            .expect("echo uses at most 4 operations")
    };
    sub(
        io,
        T_ACCEPT,
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    );
    sub(io, T_CONNECT, Op::Connect { sock: CLIENT, addr });

    let mut stats = EchoStats {
        sends: 0,
        recvs: 0,
        ticks: 0,
    };
    // Client: bytes fully handed to send, position in current chunk, bytes verified.
    let (mut sent, mut c_off, mut c_len, mut recvd) = (0u64, 0u32, 0u32, 0u64);
    // Server: range of its buffer still to echo.
    let (mut s_off, mut s_len) = (0u32, 0u32);
    let mut closing = false;

    loop {
        io.tick(true, out).map_err(|_| EchoError::Driver)?;
        stats.ticks += 1;
        while let Some(c) = out.pop() {
            let t = c.token.0;
            if c.result < 0 {
                return Err(EchoError::Io(t, c.result));
            }
            let n = c.result as u32;
            match t {
                T_ACCEPT => sub(
                    io,
                    T_SRECV,
                    Op::Recv {
                        sock: SERVER,
                        buf: SERVER_BUF,
                        off: 0,
                        len: chunk,
                    },
                ),
                T_CONNECT => {
                    sub(
                        io,
                        T_CRECV,
                        Op::Recv {
                            sock: CLIENT,
                            buf: CLIENT_RX,
                            off: 0,
                            len: chunk,
                        },
                    );
                    c_len = fill(io, sent, total, chunk);
                    c_off = 0;
                    sub(
                        io,
                        T_CSEND,
                        Op::Send {
                            sock: CLIENT,
                            buf: CLIENT_TX,
                            off: 0,
                            len: c_len,
                            zero_copy,
                        },
                    );
                }
                T_CSEND => {
                    stats.sends += 1;
                    c_off += n;
                    if c_off == c_len {
                        sent += c_len as u64;
                        c_off = 0;
                        c_len = fill(io, sent, total, chunk);
                    }
                    if c_len > 0 {
                        sub(
                            io,
                            T_CSEND,
                            Op::Send {
                                sock: CLIENT,
                                buf: CLIENT_TX,
                                off: c_off,
                                len: c_len - c_off,
                                zero_copy,
                            },
                        );
                    }
                }
                T_CRECV => {
                    stats.recvs += 1;
                    let got = &io.buf(CLIENT_RX)[..n as usize];
                    if let Some(k) = got
                        .iter()
                        .enumerate()
                        .position(|(k, b)| *b != pattern(recvd + k as u64))
                    {
                        return Err(EchoError::Mismatch(recvd + k as u64));
                    }
                    recvd += n as u64;
                    if recvd < total && n > 0 {
                        sub(
                            io,
                            T_CRECV,
                            Op::Recv {
                                sock: CLIENT,
                                buf: CLIENT_RX,
                                off: 0,
                                len: chunk,
                            },
                        );
                    } else if !closing {
                        closing = true;
                        sub(io, T_CLOSE, Op::Close { sock: CLIENT });
                    }
                }
                T_SRECV => {
                    if n > 0 {
                        (s_off, s_len) = (0, n);
                        sub(
                            io,
                            T_SSEND,
                            Op::Send {
                                sock: SERVER,
                                buf: SERVER_BUF,
                                off: 0,
                                len: n,
                                zero_copy,
                            },
                        );
                    }
                }
                T_SSEND => {
                    s_off += n;
                    if s_off < s_len {
                        sub(
                            io,
                            T_SSEND,
                            Op::Send {
                                sock: SERVER,
                                buf: SERVER_BUF,
                                off: s_off,
                                len: s_len - s_off,
                                zero_copy,
                            },
                        );
                    } else {
                        sub(
                            io,
                            T_SRECV,
                            Op::Recv {
                                sock: SERVER,
                                buf: SERVER_BUF,
                                off: 0,
                                len: chunk,
                            },
                        );
                    }
                }
                T_CLOSE => {
                    return if recvd == total {
                        Ok(stats)
                    } else {
                        Err(EchoError::Mismatch(recvd))
                    }
                }
                _ => unreachable!("unknown token"),
            }
        }
    }
}

/// Write the next chunk of the pattern into the client's send buffer.
fn fill<IO: Io>(io: &mut IO, sent: u64, total: u64, chunk: u32) -> u32 {
    let n = (total - sent).min(chunk as u64) as u32;
    for (k, b) in io.buf(CLIENT_TX)[..n as usize].iter_mut().enumerate() {
        *b = pattern(sent + k as u64);
    }
    n
}
