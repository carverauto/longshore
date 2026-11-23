// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Failing forms of the calls the contract checks make.
//!
//! The contract checks (`contract`, `contract_chains`, `contract_files`,
//! `contract_net`) are tests that ship in the library, so that every backend
//! and every crate's test suite runs the same ones. A call a check needs to
//! succeed fails the check when it does not: these forms say which call it
//! was, and [`fail`] is the one place a check stops.

use crate::io::{Completions, Io, Op, Sock, Token};
use core::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

/// Stop the running contract check with `why`.
#[track_caller]
#[expect(
    clippy::panic,
    reason = "a contract check is a test: stopping it with a message is how it fails"
)]
pub(crate) fn fail(why: fmt::Arguments<'_>) -> ! {
    panic!("{why}")
}

/// The value a check needs to be there, or fail the check naming `what`.
#[track_caller]
pub(crate) fn must<T>(value: Option<T>, what: &str) -> T {
    match value {
        Some(v) => v,
        None => fail(format_args!("{what}: missing")),
    }
}

/// [`Io`] calls that fail the running check when they are refused.
pub(crate) trait Must: Io {
    fn must_submit(&mut self, token: Token, op: Op);
    fn must_submit_chain(&mut self, ops: &[(Token, Op)]);
    fn must_tick(&mut self, wait: bool, out: &mut Completions);
    fn must_listen(&mut self, sock: Sock, addr: SocketAddr) -> SocketAddr;
    fn must_listen_unix(&mut self, sock: Sock, path: &str);
    fn must_waker(&mut self) -> Arc<dyn crate::io::Wake>;
}

impl<IO: Io + ?Sized> Must for IO {
    #[track_caller]
    fn must_submit(&mut self, token: Token, op: Op) {
        if let Err(e) = self.submit(token, op) {
            fail(format_args!("submit {token:?} {op:?}: {e:?}"));
        }
    }

    #[track_caller]
    fn must_submit_chain(&mut self, ops: &[(Token, Op)]) {
        if let Err(e) = self.submit_chain(ops) {
            fail(format_args!("submit_chain of {}: {e:?}", ops.len()));
        }
    }

    #[track_caller]
    fn must_tick(&mut self, wait: bool, out: &mut Completions) {
        if let Err(e) = self.tick(wait, out) {
            fail(format_args!("tick: {e}"));
        }
    }

    #[track_caller]
    fn must_listen(&mut self, sock: Sock, addr: SocketAddr) -> SocketAddr {
        match self.listen(sock, addr) {
            Ok(bound) => bound,
            Err(e) => fail(format_args!("listen on {addr}: {e}")),
        }
    }

    #[track_caller]
    fn must_listen_unix(&mut self, sock: Sock, path: &str) {
        if let Err(e) = self.listen_unix(sock, path) {
            fail(format_args!("listen_unix on {path}: {e}"));
        }
    }

    #[track_caller]
    fn must_waker(&mut self) -> Arc<dyn crate::io::Wake> {
        must(self.waker(), "waker")
    }
}
