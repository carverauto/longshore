// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The two ends of a benchmark run: the peer, which answers a frame that
//! asks for it and counts every frame, and the load, which keeps one
//! request in flight on every connection, or sends frames as fast as they
//! are taken.

use super::path::{LOG, REPLICA};
use super::{queued, thread_cpu, unexpected, Kind, Opts, Peer, Side, HEADER};
use crate::die::OrDie;
use longshore::io::Io;
use longshore::tcp::{Conn, Event, SendError};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The byte behind the length prefix: 1 asks for the frame to be sent back.
const ASKS: usize = 4;

/// Accept, count, and send back what asks for it, until told to stop.
pub(super) fn serve<IO: Io>(side: &mut Side<IO>, stop: &AtomicBool) -> Peer {
    let mut seen = Peer::default();
    let cpu0 = thread_cpu();
    while !stop.load(Ordering::Acquire) {
        side.turn();
        while let Some(e) = side.net.next_event() {
            let conn = match e {
                Event::Readable(conn) => conn,
                Event::Accepted { .. } | Event::Writable(_) | Event::Closed { .. } => continue,
                Event::Up(_)
                | Event::Refused { .. }
                | Event::Finished(_)
                | Event::Down { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => unexpected(e),
            };
            while let Ok(Some(frame)) = side.net.next_frame(&mut side.io, conn) {
                seen.frames += 1;
                seen.bytes += u64::from(frame.len());
                let asks = side
                    .net
                    .bytes(&mut side.io, &frame)
                    .is_some_and(|b| b.get(ASKS) == Some(&1));
                if !asks {
                    side.net.release(frame);
                    continue;
                }
                // Into frame memory if it is not there, and back from it.
                // An answer that cannot be sent would be a request never
                // answered and never counted: that ends the run. A
                // connection the load has closed at its end is passed over.
                let held = side
                    .net
                    .hold(&mut side.io, frame)
                    .ok()
                    .or_die("frame memory");
                match side.net.send_lent(conn, &held) {
                    Ok(()) | Err(SendError::NotUp | SendError::Stale) => {}
                    Err(fault @ (SendError::Full | SendError::TooLarge | SendError::NotHeld)) => {
                        crate::die::die(format_args!("answering on {conn:?}: {fault:?}"))
                    }
                }
                side.net.release(held);
            }
        }
    }
    seen.cpu = thread_cpu() - cpu0;
    seen
}

/// What the load measured.
pub(super) struct Done {
    /// Requests answered, or frames queued.
    count: u64,
    /// Answer times in nanoseconds, by power of two and sixteenth.
    times: Box<[u64]>,
    /// How long was measured, from every connection up to the last turn,
    /// and the CPU time of the thread in it.
    secs: f64,
    cpu: f64,
}

impl Done {
    fn record(&mut self, ns: u64) {
        let ns = ns.max(1);
        let power = 63 - u64::from(ns.leading_zeros());
        let sixteenth = if power >= 4 {
            (ns >> (power - 4)) & 15
        } else {
            0
        };
        self.times[(power * 16 + sixteenth) as usize] += 1;
        self.count += 1;
    }

    /// The time below which `per_mille` thousandths of the answers came,
    /// in microseconds.
    fn percentile(&self, per_mille: u64) -> f64 {
        let want = (self.count * per_mille).div_ceil(1000);
        let mut below = 0;
        for (at, n) in self.times.iter().enumerate() {
            below += n;
            if below >= want && *n > 0 {
                let (power, sixteenth) = (at as u64 / 16, at as u64 % 16);
                let low = (1u64 << power) + ((sixteenth << power) >> 4);
                return low as f64 / 1000.0;
            }
        }
        0.0
    }
}

/// One connection of the load: when its request went, and whether a
/// frame waits for room to be sent.
#[derive(Clone, Copy)]
struct Line {
    conn: Conn,
    sent: Option<Instant>,
    waits: bool,
    /// One of the three a log node sends every block on to.
    replica: bool,
}

/// Send one frame of the run on `line`; whether it was queued.
fn send<IO: Io>(side: &mut Side<IO>, o: &Opts, line: &mut Line) -> bool {
    // A replica says once what it is, in a frame of eight bytes.
    let (len, asks) = match o.kind {
        _ if line.replica => (8, REPLICA),
        Kind::Rr => (o.frame_len(), 1),
        Kind::Bulk => (o.frame_len(), 0),
        Kind::Path => (o.frame_len(), LOG),
    };
    let sent = side.net.send_frame(&mut side.io, line.conn, len, |to| {
        to[..4].copy_from_slice(&(len - 4).to_be_bytes());
        to[ASKS] = asks;
    });
    line.waits = !queued(sent, line.conn);
    if !line.waits && o.kind != Kind::Bulk && !line.replica {
        line.sent = Some(Instant::now());
    }
    !line.waits
}

/// Open the connections of the run and wait until every one is up; a
/// replica says what it is as soon as it is.
fn open_lines<IO: Io>(side: &mut Side<IO>, o: &Opts, addr: SocketAddr) -> Vec<Line> {
    // The replicas of a log node first, so that they are there when the
    // first block is durable.
    let replicas = o.lines() - o.conns;
    let mut lines: Vec<Line> = (0..o.lines())
        .map(|at| Line {
            conn: side
                .net
                .connect(&mut side.io, addr, o.conn_opts())
                .ok()
                .or_die("connect"),
            sent: None,
            waits: false,
            replica: at < replicas,
        })
        .collect();
    let (mut up, patience) = (0, Instant::now());
    while up < lines.len() {
        if patience.elapsed() > Duration::from_secs(30) {
            crate::die::die(format_args!("{up} of {} connections came up", lines.len()));
        }
        side.turn();
        while let Some(e) = side.net.next_event() {
            let Event::Up(conn) = e else { unexpected(e) };
            up += 1;
            let line = lines.get_mut(conn.slot() as usize).filter(|l| l.replica);
            if let Some(replica) = line {
                let _said: bool = send(side, o, replica);
            }
        }
    }
    lines
}

/// Something came for `line`, or it can send again: take the answers,
/// and send what the run sends next.
fn serve_line<IO: Io>(side: &mut Side<IO>, o: &Opts, line: &mut Line, done: &mut Done) {
    let mut answered = false;
    while let Ok(Some(frame)) = side.net.next_frame(&mut side.io, line.conn) {
        side.net.release(frame);
        if let Some(at) = line.sent.take() {
            done.record(at.elapsed().as_nanos() as u64);
            answered = true;
        }
    }
    if line.replica {
        // It only receives, once it has said what it is.
        if line.waits {
            let _said: bool = send(side, o, line);
        }
    } else if o.kind == Kind::Bulk {
        // As many as are taken: the next Writable brings more.
        while send(side, o, line) {
            done.count += 1;
        }
    } else if answered || line.waits {
        let _queued: bool = send(side, o, line);
    }
}

/// Open the connections, run the load for the time asked, close them.
/// The time measured is from every connection up to the last turn.
pub(super) fn drive<IO: Io>(side: &mut Side<IO>, o: &Opts, addr: SocketAddr) -> Done {
    let mut lines = open_lines(side, o, addr);
    let mut done = Done {
        count: 0,
        times: vec![0; 64 * 16].into_boxed_slice(),
        secs: 0.0,
        cpu: 0.0,
    };
    let (started, cpu0) = (Instant::now(), thread_cpu());
    for line in lines.iter_mut().filter(|l| !l.replica) {
        while send(side, o, line) && o.kind == Kind::Bulk {
            done.count += 1;
        }
    }
    while started.elapsed() < Duration::from_secs(o.secs) {
        side.turn();
        while let Some(e) = side.net.next_event() {
            let conn = match e {
                Event::Writable(conn) | Event::Readable(conn) => conn,
                Event::Up(_)
                | Event::Accepted { .. }
                | Event::Refused { .. }
                | Event::Finished(_)
                | Event::Down { .. }
                | Event::Closed { .. }
                | Event::ListenFailed { .. }
                | Event::Drained => unexpected(e),
            };
            // By its slot: the load's own bookkeeping must not grow with
            // the number of connections it measures.
            let line = lines.get_mut(conn.slot() as usize);
            if let Some(known) = line.filter(|l| l.conn == conn) {
                serve_line(side, o, known, &mut done);
            }
        }
    }
    (done.secs, done.cpu) = (started.elapsed().as_secs_f64(), thread_cpu() - cpu0);
    for line in &lines {
        let _closing = side.net.close(&mut side.io, line.conn);
    }
    side.turn();
    done
}

/// One JSON line for the run.
pub(super) fn report(o: &Opts, done: &Done, peer: &Peer) {
    let (secs, cpu) = (done.secs, done.cpu);
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let gb = peer.bytes as f64 / 1e9;
    let per_gb = |spent: f64| if gb > 0.0 { spent / gb } else { 0.0 };
    let frame = o.frame_len();
    let count = if o.kind == Kind::Bulk {
        peer.frames
    } else {
        done.count
    };
    let (cmd, sync) = match o.kind {
        Kind::Rr => ("rr", "-"),
        Kind::Bulk => ("bulk", "-"),
        Kind::Path if o.dsync => ("path", "dsync"),
        Kind::Path => ("path", "fsync"),
    };
    println!(
        "{{\"cmd\":\"{cmd}\",\"sync\":\"{sync}\",\"backend\":\"{}\",\"rx\":\"{:?}\",\"wait\":\"{}\",\"conns\":{},\"frame\":{frame},\"header\":{HEADER},\"ring\":{},\"zc_min\":{},\"segment\":{},\"secs\":{secs:.2},\"count\":{count},\"per_s\":{:.0},\"p50_us\":{:.1},\"p99_us\":{:.1},\"p999_us\":{:.1},\"gb_per_s\":{:.3},\"cpu\":{cpu:.2},\"peer_cpu\":{:.2},\"cpu_s_per_gb\":{:.3},\"peer_cpu_s_per_gb\":{:.3},\"arch\":\"{}\",\"kernel\":\"{}\"}}",
        if o.epoll { "epoll" } else { "uring" },
        o.rx,
        if o.spin { "spin" } else { "sleep" },
        o.conns,
        o.ring,
        o.zc_min,
        o.segment,
        count as f64 / secs,
        done.percentile(500),
        done.percentile(990),
        done.percentile(999),
        gb / secs,
        peer.cpu,
        per_gb(cpu),
        per_gb(peer.cpu),
        std::env::consts::ARCH,
        kernel.trim(),
    );
}
