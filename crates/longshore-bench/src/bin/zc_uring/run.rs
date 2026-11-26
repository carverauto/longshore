// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A run of `zc_uring`: one connection received into pages, and with
//! `log` its whole pages written to a file.

use super::opts::{
    limits, parse, submit, thread_cpu, Opts, ACCEPT, CANCEL, LOG, OPEN, PAGE, RECV, WRITE,
};
use crate::die::{die, OrDie};
use longshore::io::uring::UringIo;
use longshore::io::{
    BufId, Completion, Completions, Io, Limits, Op, OpenFlags, Sock, Token, ECANCELED, ENOMEM,
    ENOSPC,
};
use std::collections::VecDeque;
use std::time::Instant;

/// What a run saw.
#[derive(Default)]
struct Counts {
    frames: u64,
    prefix_wrong: u64,
    armings: u64,
    exact: u64,
    last_zero: u64,
    pieces: u64,
    at_page_start: u64,
    bytes: u64,
    written: u64,
}

/// Where the receive is in the frame it is receiving.
struct Frame {
    len: u32,
    at: u32,
    prefix: [u8; 4],
}

impl Frame {
    fn eat(&mut self, mut bytes: &[u8], c: &mut Counts) {
        while !bytes.is_empty() {
            let take = bytes.len().min((self.len - self.at) as usize);
            // Only the length is read: reading every byte would cost what
            // the copy this mode saves does.
            let need = 4usize.saturating_sub(self.at as usize).min(take);
            if need > 0 {
                let at = self.at as usize;
                self.prefix[at..at + need].copy_from_slice(&bytes[..need]);
            }
            let end = self.at + take as u32;
            if self.at < 4 && end >= 4 && u32::from_be_bytes(self.prefix) != self.len - 4 {
                c.prefix_wrong += 1;
            }
            self.at = if end == self.len { 0 } else { end };
            c.frames += u64::from(end == self.len);
            bytes = &bytes[take..];
        }
    }

    /// The credit of the next arming: to the end of the first 256
    /// bytes, or to the end of the frame.
    fn credit(&self) -> u32 {
        if self.at < 256 {
            256 - self.at
        } else {
            self.len - self.at
        }
    }
}

/// The log: whole pages waiting for a write, and those a write holds.
struct Log {
    waiting: VecDeque<BufId>,
    rows: Vec<Vec<BufId>>,
    free: Vec<u32>,
    file_off: u64,
}

struct Run {
    io: UringIo,
    limits: Limits,
    frame: Frame,
    counts: Counts,
    log: Option<Log>,
    /// The receive armed: its credit, and the bytes it brought so far.
    armed: Option<(u32, u32)>,
    cancelling: bool,
    connected: bool,
    finished: bool,
    /// Pages the log may hold before the receive waits for its writes:
    /// half the area, so that the card is never left without.
    budget: usize,
}

impl Run {
    fn piece(&mut self, c: Completion) {
        let (page, off, len) = (
            c.buf().or_die("a piece's page"),
            c.off(),
            c.result().unsigned_abs(),
        );
        let bytes = &self.io.buf(page)[off as usize..(off + len) as usize];
        self.frame.eat(bytes, &mut self.counts);
        self.counts.pieces += 1;
        self.counts.at_page_start += u64::from(off == 0);
        self.counts.bytes += u64::from(len);
        if let Some((_, got)) = self.armed.as_mut() {
            *got += len;
        }
        match self.log.as_mut() {
            Some(log) if off == 0 && len == PAGE => log.waiting.push_back(page),
            _ => self.io.give_back_piece(page, off, len),
        }
    }

    fn end_of_receive(&mut self, result: i32) {
        let (credit, got) = self.armed.take().or_die("a receive armed");
        self.cancelling = false;
        match result {
            0 if got == credit => {
                (self.counts.exact, self.counts.last_zero) =
                    (self.counts.exact + 1, self.counts.last_zero + 1)
            }
            0 => (self.finished, self.counts.last_zero) = (true, self.counts.last_zero + 1),
            // Delivered is good; the rest is asked for again.
            r if r == -ENOSPC || r == -ENOMEM || r == -ECANCELED => {}
            r => die(format_args!("the receive ended with {r}")),
        }
    }

    fn written(&mut self, row: u32, result: i32) {
        let log = self.log.as_mut().or_die("a log");
        let pages = &mut log.rows[row as usize];
        if result != (pages.len() as u32 * PAGE).cast_signed() {
            die(format_args!(
                "a gathered write of {} pages gave {result}",
                pages.len()
            ));
        }
        self.counts.written += u64::from(result.unsigned_abs());
        for page in pages.drain(..) {
            self.io.give_back_piece(page, 0, PAGE);
        }
        log.free.push(row);
    }

    fn handle(&mut self, c: Completion) {
        match c.token().get() {
            RECV if c.more() => self.piece(c),
            RECV => self.end_of_receive(c.result()),
            ACCEPT if c.result() < 0 => die(format_args!("accept gave {}", c.result())),
            ACCEPT => self.connected = true,
            OPEN if c.result() < 0 => die(format_args!("opening the log gave {}", c.result())),
            OPEN | CANCEL => {}
            t if t >= WRITE => self.written((t - WRITE) as u32, c.result()),
            t => die(format_args!("a completion for token {t}")),
        }
    }

    /// Start a gathered write in every free row, while pages wait.
    fn write(&mut self) {
        let Some(log) = self.log.as_mut() else { return };
        while !log.waiting.is_empty() {
            let Some(row) = log.free.pop() else { return };
            let pages = &mut log.rows[row as usize];
            let n = log.waiting.len().min(pages.capacity());
            pages.extend(log.waiting.drain(..n));
            let list = self.limits.small(row);
            let at = self.io.buf(list);
            for (k, page) in pages.iter().enumerate() {
                let words = [page.get(), 0, PAGE];
                for (w, word) in words.iter().enumerate() {
                    at[k * 12 + w * 4..k * 12 + w * 4 + 4].copy_from_slice(&word.to_le_bytes());
                }
            }
            let len = n as u32 * PAGE;
            let op = Op::FileWriteSpans {
                slot: LOG,
                list,
                list_off: 0,
                count: n as u32,
                len,
                file_off: log.file_off,
            };
            log.file_off += u64::from(len);
            submit(&mut self.io, WRITE + u64::from(row), op);
        }
    }

    fn drained(&self) -> bool {
        self.log
            .as_ref()
            .is_none_or(|l| l.waiting.is_empty() && l.free.len() == l.rows.len())
    }

    fn turn(&mut self, out: &mut Completions) {
        self.io.tick(true, out).or_die("tick");
        while let Some(c) = out.pop() {
            self.handle(c);
        }
        if self.io.take_ring_full() && self.armed.is_some() && !self.cancelling {
            // A receive with credit left may stay silent: cancel it, and
            // arm it again for the rest.
            let cancel = Op::Cancel {
                target: Token::new(RECV),
            };
            submit(&mut self.io, CANCEL, cancel);
            self.cancelling = true;
        }
        self.write();
        let held = self.log.as_ref().map_or(0, |l| {
            l.waiting.len() + l.rows.iter().map(Vec::len).sum::<usize>()
        });
        let room = held < self.budget;
        if self.connected && !self.finished && self.armed.is_none() && room {
            let credit = self.frame.credit();
            let recv = Op::ArmedRecvZc {
                sock: Sock::new(1),
                credit,
            };
            submit(&mut self.io, RECV, recv);
            self.armed = Some((credit, 0));
            self.counts.armings += 1;
        }
    }
}

/// Open the log at `path`, its name in the small buffer after the lists.
fn open_log(run: &mut Run, path: &str, out: &mut Completions) {
    let (path_buf, b) = (run.limits.small(run.limits.gather_ops()), path.as_bytes());
    run.io.buf(path_buf)[..=b.len()].copy_from_slice(&[b, &[0]].concat());
    let flags = OpenFlags::NONE.with_write(true).with_create(true);
    let (path_off, path_len) = (0, b.len() as u32 + 1);
    let op = Op::FileOpen {
        slot: LOG,
        path_buf,
        path_off,
        path_len,
        flags: flags.with_direct(true),
    };
    submit(&mut run.io, OPEN, op);
    run.io.tick(true, out).or_die("tick");
    while let Some(c) = out.pop() {
        run.handle(c);
    }
}

fn report(o: &Opts, c: &Counts, secs: f64, cpu: f64) {
    let named = [
        ("frames", c.frames),
        ("prefix_wrong", c.prefix_wrong),
        ("armings", c.armings),
        ("exact_armings", c.exact),
        ("last_zero", c.last_zero),
        ("pieces", c.pieces),
        ("pieces_at_page_start", c.at_page_start),
        ("bytes", c.bytes),
    ];
    let mut line: Vec<String> = named.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    if o.mode == "log" {
        let gb = c.bytes as f64 / 1e9;
        line.push(format!("\"written\":{},\"secs\":{secs:.3}", c.written));
        line.push(format!("\"gb_per_s\":{:.3}", gb / secs.max(1e-9)));
        line.push(format!("\"cpu_s_per_gb\":{:.4}", cpu / gb.max(1e-9)));
    }
    println!("{{{}}}", line.join(","));
}

pub(super) fn main() {
    let o = parse();
    let limits = limits(&o);
    let io = match UringIo::new(limits) {
        Ok(_) if o.mode == "refuse" => die("the area registered: nothing was refused"),
        Err(e) if o.mode == "refuse" => return println!("{{\"refused\":{:?}}}", e.to_string()),
        r => r.or_die("UringIo::new"),
    };
    let log = (o.mode == "log").then(|| Log {
        waiting: VecDeque::with_capacity(o.pages as usize),
        rows: (0..o.writes)
            .map(|_| Vec::with_capacity(o.spans as usize))
            .collect(),
        free: (0..o.writes).rev().collect(),
        file_off: 0,
    });
    let frame = Frame {
        len: o.frame,
        at: 0,
        prefix: [0; 4],
    };
    let (counts, armed) = (Counts::default(), None);
    let (cancelling, connected, finished) = (false, false, false);
    let mut run = Run {
        io,
        limits,
        frame,
        counts,
        log,
        armed,
        cancelling,
        connected,
        finished,
        budget: o.pages as usize / 2,
    };
    let mut out = Completions::new(limits.ops_max() as usize + limits.area_returns() as usize);
    if let Some(path) = o.file.as_deref().filter(|_| o.mode == "log") {
        open_log(&mut run, path, &mut out);
    }
    let at = run.io.listen(Sock::new(0), o.addr).or_die("listen");
    eprintln!("zc_uring: listening on {at}");
    let accept = Op::Accept {
        listener: Sock::new(0),
        sock: Sock::new(1),
    };
    submit(&mut run.io, ACCEPT, accept);
    let mut start = None;
    while !(run.finished && run.drained()) {
        run.turn(&mut out);
        if start.is_none() && run.counts.bytes > 0 {
            start = Some((Instant::now(), thread_cpu()));
        }
    }
    let since = |(t, c): (Instant, f64)| (t.elapsed().as_secs_f64(), thread_cpu() - c);
    let (secs, cpu) = start.map_or((0.0, 0.0), since);
    report(&o, &run.counts, secs, cpu);
}
