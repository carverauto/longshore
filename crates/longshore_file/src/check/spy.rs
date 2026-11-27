// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A backend seen from outside the set: every operation the set submits,
//! every completion the backend returns, reads of the clock, and room the
//! check takes away so that the set has to wait.

use super::kind::{describe, Kind};
use core::cell::Cell;
use longshore_common::io::{
    BufId, Busy, Completion, Completions, FileId, FileSlot, Io, Op, Sock, Token,
};
use std::io;
use std::net::SocketAddr;

/// The most completions one `tick` of a check may hold.
pub const STAGED: usize = 64;

/// One operation given to the backend.
#[derive(Clone, Copy, Debug)]
pub struct Submitted {
    number: u64,
    token: Token,
    op: Op,
    chain: Option<(u64, u32, u32)>,
}

impl Submitted {
    /// Its number among every operation the backend was given, from 0.
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Its token.
    pub const fn token(&self) -> Token {
        self.token
    }

    /// The operation.
    pub const fn op(&self) -> Op {
        self.op
    }

    /// For a member of a chain: the chain's number, the member's place in
    /// it, and the chain's length.
    pub const fn chain(&self) -> Option<(u64, u32, u32)> {
        self.chain
    }

    /// Which operation, of which file slot, at which file offset.
    pub fn describe(&self) -> (Kind, Option<FileSlot>, Option<u64>) {
        describe(&self.op)
    }
}

/// A backend wrapped so that a check sees what the set does with it.
/// Its record is a ring of fixed length, allocated once, here.
pub struct Spy<IO> {
    inner: IO,
    log: Vec<Option<Submitted>>,
    results: Vec<Option<(Token, i32)>>,
    submitted: u64,
    completed: u64,
    chains: u64,
    refused: u64,
    clock_reads: Cell<u64>,
    ticks: u64,
    room: Option<u32>,
    cut_reads: u32,
}

impl<IO: Io> Spy<IO> {
    /// `inner`, remembering the last `keep` operations and completions.
    pub fn new(inner: IO, keep: usize) -> Spy<IO> {
        Spy {
            inner,
            log: vec![None; keep.max(1)],
            results: vec![None; keep.max(1)],
            submitted: 0,
            completed: 0,
            chains: 0,
            refused: 0,
            clock_reads: Cell::new(0),
            ticks: 0,
            room: None,
            cut_reads: 0,
        }
    }

    /// The backend.
    pub fn inner(&self) -> &IO {
        &self.inner
    }

    /// The backend, to change it (a simulator's faults, its power).
    pub fn inner_mut(&mut self) -> &mut IO {
        &mut self.inner
    }

    /// Operations given to the backend so far.
    pub const fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Completions the backend returned so far.
    pub const fn completed(&self) -> u64 {
        self.completed
    }

    /// Operations given and not yet completed.
    pub const fn outstanding(&self) -> u64 {
        self.submitted - self.completed
    }

    /// Submissions refused for want of room, by [`Spy::set_room`].
    pub const fn refused(&self) -> u64 {
        self.refused
    }

    /// Reads of the clock (`now_ns`, `unix_secs`) so far.
    pub fn clock_reads(&self) -> u64 {
        self.clock_reads.get()
    }

    /// Calls of `tick` so far.
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Cut the next `n` reads that complete with two bytes or more to half
    /// of what they read, as a kernel may.
    pub fn cut_reads(&mut self, n: u32) {
        self.cut_reads = n;
    }

    /// The operation last given under `token`, while the record holds it.
    fn op_of(&self, token: Token) -> Option<Op> {
        (0..self.submitted)
            .rev()
            .take(self.log.len())
            .filter_map(|n| self.at(n))
            .find(|s| s.token == token)
            .map(|s| s.op)
    }

    /// A completion as the backend gave it, or a read's cut to half its
    /// bytes while reads are to be cut: a short read before the end of the
    /// file, which a kernel may return and the simulator never does.
    fn cut(&mut self, c: Completion) -> Completion {
        if self.cut_reads == 0 || c.result() < 2 {
            return c;
        }
        if !matches!(self.op_of(c.token()), Some(Op::FileRead { .. })) {
            return c;
        }
        self.cut_reads -= 1;
        Completion::new(c.token(), c.result() / 2).with_more(c.more())
    }

    /// The backend lost power: what was in flight will never complete.
    pub fn lost(&mut self) {
        self.completed = self.submitted;
    }

    /// Room for `n` more operations, then `Busy`; `None` for the backend's
    /// own room.
    pub fn set_room(&mut self, room: Option<u32>) {
        self.room = room;
    }

    /// Operation number `n`, while the record still holds it.
    pub fn at(&self, n: u64) -> Option<Submitted> {
        let s = self.log[(n % self.log.len() as u64) as usize]?;
        (s.number == n).then_some(s)
    }

    /// The operations given from number `from` on, oldest first, as far as
    /// the record still holds them.
    pub fn since(&self, from: u64) -> impl Iterator<Item = Submitted> + '_ {
        (from..self.submitted).filter_map(|n| self.at(n))
    }

    /// Completion number `n` (from 0): its token and result.
    pub fn result(&self, n: u64) -> Option<(Token, i32)> {
        if n >= self.completed || self.completed - n > self.results.len() as u64 {
            return None;
        }
        self.results[(n % self.results.len() as u64) as usize]
    }

    fn take_room(&mut self, n: u32) -> Result<(), Busy> {
        match self.room {
            Some(left) if left < n => {
                self.refused += 1;
                Err(Busy)
            }
            Some(left) => {
                self.room = Some(left - n);
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn record(&mut self, token: Token, op: Op, chain: Option<(u64, u32, u32)>) {
        let n = self.submitted;
        let at = (n % self.log.len() as u64) as usize;
        self.log[at] = Some(Submitted {
            number: n,
            token,
            op,
            chain,
        });
        self.submitted += 1;
    }
}

impl<IO: Io> Io for Spy<IO> {
    fn now_ns(&self) -> u64 {
        self.clock_reads.set(self.clock_reads.get() + 1);
        self.inner.now_ns()
    }

    fn unix_secs(&self) -> u64 {
        self.clock_reads.set(self.clock_reads.get() + 1);
        self.inner.unix_secs()
    }

    fn buf(&mut self, id: BufId) -> &mut [u8] {
        self.inner.buf(id)
    }

    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.inner.buf_pair(a, b)
    }

    fn register_file(&mut self, path: &str) -> io::Result<FileId> {
        self.inner.register_file(path)
    }

    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> io::Result<SocketAddr> {
        self.inner.listen(sock, addr)
    }

    fn listen_unix(&mut self, sock: Sock, path: &str) -> io::Result<()> {
        self.inner.listen_unix(sock, path)
    }

    fn give_back(&mut self, id: BufId) {
        self.inner.give_back(id);
    }

    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        self.take_room(1)?;
        self.inner.submit(token, op)?;
        self.record(token, op, None);
        Ok(())
    }

    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), Busy> {
        self.take_room(ops.len() as u32)?;
        self.inner.submit_chain(ops)?;
        let chain = self.chains;
        self.chains += 1;
        for (i, (token, op)) in ops.iter().enumerate() {
            self.record(*token, *op, Some((chain, i as u32, ops.len() as u32)));
        }
        Ok(())
    }

    fn tick(&mut self, wait: bool, out: &mut Completions) -> io::Result<()> {
        self.ticks += 1;
        let before = out.len();
        self.inner.tick(wait, out)?;
        // Take every completion out, in order, record the new ones (those
        // after the first `before`), and put them all back as they were.
        let mut staged = [None::<Completion>; STAGED];
        let mut n = 0;
        while n < STAGED {
            match out.pop() {
                Some(c) => staged[n] = Some(c),
                None => break,
            }
            n += 1;
        }
        for (i, got) in staged[..n].iter().flatten().enumerate() {
            let c = if i >= before { self.cut(*got) } else { *got };
            if i >= before {
                let at = (self.completed % self.results.len() as u64) as usize;
                self.results[at] = Some((c.token(), c.result()));
                if !c.more() {
                    self.completed += 1;
                }
            }
            out.push(c);
        }
        Ok(())
    }
}
