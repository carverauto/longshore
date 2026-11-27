// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A socket whose sends end a buffer each (`Io::end_sends`) still sends:
//! on Linux with `MSG_EOR`, on macOS, whose TCP refuses the flag, with
//! nothing.

use super::{pair, LIMITS};
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{BufId, Completions, Io, Op, Sock, Token};

#[test]
fn a_socket_whose_sends_end_a_buffer_each_still_sends() {
    let mut io = PosixIo::new(LIMITS).unwrap();
    let mut out = Completions::new(8);
    let (listener, server, client) = (Sock::new(0), Sock::new(1), Sock::new(2));
    pair(&mut io, &mut out, listener, server, client);
    io.end_sends(client, true);
    io.buf(BufId::new(0))[..5].copy_from_slice(b"hello");
    for zero_copy in [false, true] {
        let send = Op::Send {
            sock: client,
            buf: BufId::new(0),
            off: 0,
            len: 5,
            zero_copy,
        };
        io.submit(Token::new(10), send).unwrap();
        let recv = Op::Recv {
            sock: server,
            buf: BufId::new(1),
            off: 0,
            len: 5,
        };
        io.submit(Token::new(11), recv).unwrap();
        let mut got = within(&mut io, &mut out, 2);
        got.sort_unstable();
        assert_eq!(got, vec![(10, 5), (11, 5)], "zero copy: {zero_copy}");
        assert_eq!(&io.buf(BufId::new(1))[..5], b"hello");
    }
}

/// Up to `want` completions within two seconds: a refused send must fail
/// the test, not leave its receive waiting for ever.
fn within(io: &mut PosixIo, out: &mut Completions, want: usize) -> Vec<(u64, i32)> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut got = Vec::with_capacity(want);
    while got.len() < want && std::time::Instant::now() < until {
        io.tick(false, out).unwrap();
        while let Some(c) = out.pop() {
            got.push((c.token().get(), c.result()));
        }
    }
    got
}
