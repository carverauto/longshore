// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A simulator that records what is submitted to it and what completes,
//! in order: for a test that checks what the set gave the backend, and
//! when, rather than only what it reported.

use longshore_common::io::{BufId, Busy, Completion, Completions, FileId, Io, Op, Sock, Token};
use longshore_common::sim::SimIo;
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};

/// One thing that happened at the backend.
#[derive(Clone, Debug)]
pub enum Entry {
    /// An operation submitted on its own.
    Op(Token, Op),
    /// A chain submitted.
    Chain(Vec<(Token, Op)>),
    /// A completion delivered by a tick.
    Done(Token, i32),
}

/// A [`SimIo`] with the record of what went through it.
pub struct Rec {
    pub sim: SimIo,
    pub log: Vec<Entry>,
}

impl Rec {
    pub fn new(sim: SimIo) -> Rec {
        Rec {
            sim,
            log: Vec::new(),
        }
    }

    /// Every operation submitted, alone or in a chain, in order, with the
    /// position of its entry in the log.
    pub fn submitted(&self) -> Vec<(usize, Token, Op)> {
        let mut all = Vec::new();
        for (k, e) in self.log.iter().enumerate() {
            match e {
                Entry::Op(t, op) => all.push((k, *t, *op)),
                Entry::Chain(ops) => all.extend(ops.iter().map(|(t, op)| (k, *t, *op))),
                Entry::Done(..) => {}
            }
        }
        all
    }

    /// The position in the log of the completion of `t` submitted at
    /// position `after`, and its result, if it has come.
    pub fn done_at(&self, t: Token, after: usize) -> Option<(usize, i32)> {
        (after..self.log.len()).find_map(|k| match self.log[k] {
            Entry::Done(d, res) if d == t => Some((k, res)),
            Entry::Op(..) | Entry::Chain(..) | Entry::Done(..) => None,
        })
    }
}

impl Deref for Rec {
    type Target = SimIo;
    fn deref(&self) -> &SimIo {
        &self.sim
    }
}

impl DerefMut for Rec {
    fn deref_mut(&mut self) -> &mut SimIo {
        &mut self.sim
    }
}

impl Io for Rec {
    fn now_ns(&self) -> u64 {
        self.sim.now_ns()
    }
    fn unix_secs(&self) -> u64 {
        self.sim.unix_secs()
    }
    fn buf(&mut self, id: BufId) -> &mut [u8] {
        self.sim.buf(id)
    }
    fn buf_pair(&mut self, a: BufId, b: BufId) -> (&mut [u8], &mut [u8]) {
        self.sim.buf_pair(a, b)
    }
    fn register_file(&mut self, path: &str) -> std::io::Result<FileId> {
        self.sim.register_file(path)
    }
    fn listen(&mut self, sock: Sock, addr: SocketAddr) -> std::io::Result<SocketAddr> {
        self.sim.listen(sock, addr)
    }
    fn listen_unix(&mut self, sock: Sock, path: &str) -> std::io::Result<()> {
        self.sim.listen_unix(sock, path)
    }
    fn give_back(&mut self, id: BufId) {
        self.sim.give_back(id);
    }
    fn submit(&mut self, token: Token, op: Op) -> Result<(), Busy> {
        self.sim.submit(token, op)?;
        self.log.push(Entry::Op(token, op));
        Ok(())
    }
    fn submit_chain(&mut self, ops: &[(Token, Op)]) -> Result<(), Busy> {
        self.sim.submit_chain(ops)?;
        self.log.push(Entry::Chain(ops.to_vec()));
        Ok(())
    }
    fn tick(&mut self, wait: bool, out: &mut Completions) -> std::io::Result<()> {
        self.sim.tick(wait, out)?;
        let mut got: Vec<Completion> = Vec::new();
        while let Some(c) = out.pop() {
            got.push(c);
        }
        for c in got {
            self.log.push(Entry::Done(c.token(), c.result()));
            out.push(c);
        }
        Ok(())
    }
}
