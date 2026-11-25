// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Scenarios that run unchanged on any `Io`.

use crate::io::{BufId, Completions, Io, Op, Sock, Token};
use std::net::SocketAddr;

/// Why [`echo`] failed.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EchoError {
    /// An operation failed: (token, negated errno).
    Io(u64, i32),
    /// Echoed byte at this stream offset differed from what was sent.
    Mismatch(u64),
    /// `Io::tick`, `Io::listen` or `Io::submit` itself failed.
    Driver,
}

/// How many sends, receives and ticks [`echo`] took.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct EchoStats {
    pub(crate) sends: u64,
    pub(crate) recvs: u64,
    pub(crate) ticks: u64,
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
    addr: SocketAddr,
    total: u64,
    chunk: u32,
    zero_copy: bool,
    out: &mut Completions,
) -> Result<EchoStats, EchoError> {
    let addr = io.listen(LISTENER, addr).map_err(|_| EchoError::Driver)?;
    // Echo keeps at most 4 operations in flight; an `Io` with fewer is refused.
    sub(
        io,
        T_ACCEPT,
        Op::Accept {
            listener: LISTENER,
            sock: SERVER,
        },
    )?;
    sub(io, T_CONNECT, Op::Connect { sock: CLIENT, addr })?;

    let mut echo = Echo {
        total,
        chunk,
        zero_copy,
        stats: EchoStats {
            sends: 0,
            recvs: 0,
            ticks: 0,
        },
        sent: 0,
        c_off: 0,
        c_len: 0,
        recvd: 0,
        s_off: 0,
        s_len: 0,
        closing: false,
    };
    loop {
        io.tick(true, out).map_err(|_| EchoError::Driver)?;
        echo.stats.ticks += 1;
        while let Some(c) = out.pop() {
            let t = c.token.0;
            let Ok(n) = u32::try_from(c.result) else {
                return Err(EchoError::Io(t, c.result));
            };
            match t {
                T_ACCEPT => sub(io, T_SRECV, echo.recv(SERVER, SERVER_BUF))?,
                T_CONNECT => echo.connected(io)?,
                T_CSEND => echo.client_sent(io, n)?,
                T_CRECV => echo.client_received(io, n)?,
                T_SRECV => echo.server_received(io, n)?,
                T_SSEND => echo.server_sent(io, n)?,
                T_CLOSE => return echo.closed(),
                _ => unreachable!("unknown token"),
            }
        }
    }
}

/// Submit one echo operation; a refused submission is a driver failure.
#[inline]
fn sub<IO: Io>(io: &mut IO, t: u64, op: Op) -> Result<(), EchoError> {
    io.submit(Token(t), op).map_err(|_| EchoError::Driver)
}

/// Echo progress on both ends of the connection.
struct Echo {
    total: u64,
    chunk: u32,
    zero_copy: bool,
    stats: EchoStats,
    /// Client: bytes fully handed to send.
    sent: u64,
    /// Client: position in the current chunk, and its length.
    c_off: u32,
    c_len: u32,
    /// Client: bytes verified.
    recvd: u64,
    /// Server: range of its buffer still to echo.
    s_off: u32,
    s_len: u32,
    closing: bool,
}

impl Echo {
    #[inline]
    fn recv(&self, sock: Sock, buf: BufId) -> Op {
        Op::Recv {
            sock,
            buf,
            off: 0,
            len: self.chunk,
        }
    }

    #[inline]
    fn send(&self, sock: Sock, buf: BufId, off: u32, len: u32) -> Op {
        Op::Send {
            sock,
            buf,
            off,
            len,
            zero_copy: self.zero_copy,
        }
    }

    /// The client connected: start receiving the echo and send the first chunk.
    fn connected<IO: Io>(&mut self, io: &mut IO) -> Result<(), EchoError> {
        sub(io, T_CRECV, self.recv(CLIENT, CLIENT_RX))?;
        self.c_len = fill(io, self.sent, self.total, self.chunk);
        self.c_off = 0;
        sub(io, T_CSEND, self.send(CLIENT, CLIENT_TX, 0, self.c_len))
    }

    /// The client sent `n` bytes: continue the chunk, or refill and send the next.
    #[inline]
    fn client_sent<IO: Io>(&mut self, io: &mut IO, n: u32) -> Result<(), EchoError> {
        self.stats.sends += 1;
        self.c_off += n;
        if self.c_off == self.c_len {
            self.sent += u64::from(self.c_len);
            self.c_off = 0;
            self.c_len = fill(io, self.sent, self.total, self.chunk);
        }
        if self.c_len > 0 {
            let op = self.send(CLIENT, CLIENT_TX, self.c_off, self.c_len - self.c_off);
            sub(io, T_CSEND, op)?;
        }
        Ok(())
    }

    /// The client received `n` echoed bytes: verify them, then receive more or close.
    #[inline]
    fn client_received<IO: Io>(&mut self, io: &mut IO, n: u32) -> Result<(), EchoError> {
        self.stats.recvs += 1;
        let recvd = self.recvd;
        let got = &io.buf(CLIENT_RX)[..n as usize];
        if let Some(k) = got
            .iter()
            .enumerate()
            .position(|(k, b)| *b != pattern(recvd + k as u64))
        {
            return Err(EchoError::Mismatch(recvd + k as u64));
        }
        self.recvd += u64::from(n);
        if self.recvd < self.total && n > 0 {
            sub(io, T_CRECV, self.recv(CLIENT, CLIENT_RX))?;
        } else if !self.closing {
            self.closing = true;
            sub(io, T_CLOSE, Op::Close { sock: CLIENT })?;
        }
        Ok(())
    }

    /// The server received `n` bytes: echo them back.
    #[inline]
    fn server_received<IO: Io>(&mut self, io: &mut IO, n: u32) -> Result<(), EchoError> {
        if n > 0 {
            (self.s_off, self.s_len) = (0, n);
            sub(io, T_SSEND, self.send(SERVER, SERVER_BUF, 0, n))?;
        }
        Ok(())
    }

    /// The server echoed `n` bytes: finish the echo, or receive the next chunk.
    #[inline]
    fn server_sent<IO: Io>(&mut self, io: &mut IO, n: u32) -> Result<(), EchoError> {
        self.s_off += n;
        if self.s_off < self.s_len {
            let op = self.send(SERVER, SERVER_BUF, self.s_off, self.s_len - self.s_off);
            sub(io, T_SSEND, op)
        } else {
            sub(io, T_SRECV, self.recv(SERVER, SERVER_BUF))
        }
    }

    /// The client closed: the echo succeeded if every byte came back.
    fn closed(&self) -> Result<EchoStats, EchoError> {
        if self.recvd == self.total {
            Ok(self.stats)
        } else {
            Err(EchoError::Mismatch(self.recvd))
        }
    }
}

/// Write the next chunk of the pattern into the client's send buffer.
fn fill<IO: Io>(io: &mut IO, sent: u64, total: u64, chunk: u32) -> u32 {
    let n = (total - sent).min(u64::from(chunk)) as u32;
    for (k, b) in io.buf(CLIENT_TX)[..n as usize].iter_mut().enumerate() {
        *b = pattern(sent + k as u64);
    }
    n
}

impl EchoStats {
    /// Sends the client submitted.
    #[inline]
    pub const fn sends(self) -> u64 {
        self.sends
    }

    /// Receives the client submitted.
    #[inline]
    pub const fn recvs(self) -> u64 {
        self.recvs
    }

    /// Ticks the run took.
    #[inline]
    pub const fn ticks(self) -> u64 {
        self.ticks
    }
}
