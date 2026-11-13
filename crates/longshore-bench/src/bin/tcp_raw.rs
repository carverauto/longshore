// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Milestone 0: raw io_uring TCP send/recv throughput with no protocol code.
//!
//! The thread under test is one io_uring thread pinned to one core. The peer
//! (`sink` or `source`) uses a thread per connection so it is not the limit.
//!
//!   peer host:  tcp_raw sink   --port 9100
//!               tcp_raw source --port 9101
//!   test host:  tcp_raw send --addr HOST:9100 --mode unbatched|batched|zc|zc-fixed
//!               tcp_raw recv --addr HOST:9101 --mode unbatched|batched
//!
//! Output is one JSON object per run. GB means 10^9 bytes.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("tcp_raw needs Linux (io_uring)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use core::ffi::{c_int, c_void};
    use longshore::io::ring::Ring;
    use longshore::io::sys::*;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    extern "C" {
        fn sched_setaffinity(pid: c_int, len: usize, mask: *const u64) -> c_int;
        fn clock_gettime(clk: c_int, ts: *mut [i64; 2]) -> c_int;
        fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    }
    const CLOCK_THREAD_CPUTIME_ID: c_int = 3;
    const MSG_TRUNC: c_int = 0x20;

    fn pin(cpu: usize) {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        if unsafe { sched_setaffinity(0, core::mem::size_of_val(&mask), mask.as_ptr()) } != 0 {
            die(&format!(
                "cannot pin to cpu {cpu}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    fn thread_cpu_secs() -> f64 {
        let mut ts = [0i64; 2];
        unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        ts[0] as f64 + ts[1] as f64 / 1e9
    }

    /// Busy jiffies (everything except idle and iowait) for one CPU, from
    /// /proc/stat. Captures softirq work that thread CPU time misses.
    fn core_busy_and_total(cpu: usize) -> (u64, u64) {
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let tag = format!("cpu{cpu} ");
        let Some(line) = stat.lines().find(|l| l.starts_with(&tag)) else {
            return (0, 0);
        };
        let v: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|x| x.parse().ok())
            .collect();
        let total: u64 = v.iter().take(8).sum();
        let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
        (total - idle, total)
    }

    fn die(msg: &str) -> ! {
        eprintln!("tcp_raw: {msg}");
        std::process::exit(1)
    }

    struct Args {
        cmd: String,
        addr: String,
        port: u16,
        mode: String,
        conns: usize,
        chunk: usize,
        secs: u64,
        cpu: Option<usize>,
    }

    fn args() -> Args {
        let mut a = Args {
            cmd: String::new(),
            addr: String::new(),
            port: 9100,
            mode: "batched".into(),
            conns: 8,
            chunk: 1 << 20,
            secs: 10,
            cpu: None,
        };
        let mut it = std::env::args().skip(1);
        a.cmd = it.next().unwrap_or_else(|| die("usage: tcp_raw sink|source|send|recv [--addr H:P] [--port P] [--mode M] [--conns N] [--chunk BYTES] [--secs S] [--cpu N]"));
        while let Some(k) = it.next() {
            let v = it
                .next()
                .unwrap_or_else(|| die(&format!("missing value for {k}")));
            let num = || {
                v.parse::<usize>()
                    .unwrap_or_else(|_| die(&format!("bad number for {k}: {v}")))
            };
            match k.as_str() {
                "--addr" => a.addr = v,
                "--port" => a.port = num() as u16,
                "--mode" => a.mode = v,
                "--conns" => a.conns = num(),
                "--chunk" => a.chunk = num(),
                "--secs" => a.secs = num() as u64,
                "--cpu" => a.cpu = Some(num()),
                _ => die(&format!("unknown flag {k}")),
            }
        }
        a
    }

    pub fn main() {
        let a = args();
        match a.cmd.as_str() {
            "sink" => peer(a.port, false),
            "source" => peer(a.port, true),
            "send" => run(&a, true),
            "recv" => run(&a, false),
            other => die(&format!("unknown command {other}")),
        }
    }

    /// Multi-threaded peer. `sink` discards with MSG_TRUNC, which skips the
    /// kernel-to-user copy so the sink stays ahead of a zero-copy sender.
    fn peer(port: u16, source: bool) {
        let l = TcpListener::bind(("0.0.0.0", port))
            .unwrap_or_else(|e| die(&format!("bind {port}: {e}")));
        eprintln!(
            "tcp_raw: {} listening on {port}",
            if source { "source" } else { "sink" }
        );
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                let mut buf = vec![0x5au8; 1 << 20];
                if source {
                    while s.write_all(&buf).is_ok() {}
                } else {
                    let fd = s.as_raw_fd();
                    while unsafe { recv(fd, buf.as_mut_ptr().cast(), buf.len(), MSG_TRUNC) } > 0 {}
                }
            });
        }
    }

    fn run(a: &Args, sending: bool) {
        if let Some(c) = a.cpu {
            pin(c);
        }
        let unbatched = a.mode == "unbatched";
        let zc = sending && (a.mode == "zc" || a.mode == "zc-fixed");
        let fixed_buf = a.mode == "zc-fixed";
        match (sending, a.mode.as_str()) {
            (_, "unbatched" | "batched") | (true, "zc" | "zc-fixed") => {}
            _ => die(&format!("mode {} is not valid for {}", a.mode, a.cmd)),
        }

        let socks: Vec<TcpStream> = (0..a.conns)
            .map(|_| {
                TcpStream::connect(&a.addr)
                    .unwrap_or_else(|e| die(&format!("connect {}: {e}", a.addr)))
            })
            .collect();
        let fds: Vec<i32> = socks.iter().map(|s| s.as_raw_fd()).collect();
        let mut bufs: Vec<Vec<u8>> = (0..a.conns).map(|_| vec![0xa5u8; a.chunk]).collect();

        // The unoptimized case uses a default ring; everything else uses the
        // single-issuer flags and registered files.
        let flags = if unbatched {
            0
        } else {
            IORING_SETUP_SINGLE_ISSUER | IORING_SETUP_DEFER_TASKRUN
        };
        let mut ring = Ring::new(256, flags).unwrap_or_else(|e| die(&e.to_string()));
        if !unbatched {
            ring.register_files(&fds)
                .unwrap_or_else(|e| die(&e.to_string()));
        }
        if fixed_buf {
            let iov: Vec<iovec> = bufs
                .iter_mut()
                .map(|b| iovec {
                    iov_base: b.as_mut_ptr().cast(),
                    iov_len: b.len(),
                })
                .collect();
            ring.register_buffers(&iov)
                .unwrap_or_else(|e| die(&e.to_string()));
        }

        let make = |i: usize, bufs: &mut [Vec<u8>]| -> io_uring_sqe {
            let mut s = io_uring_sqe {
                addr: bufs[i].as_mut_ptr() as u64,
                len: a.chunk as u32,
                user_data: i as u64,
                ..Default::default()
            };
            if unbatched {
                s.fd = fds[i];
            } else {
                s.fd = i as i32;
                s.flags = IOSQE_FIXED_FILE;
            }
            if sending {
                s.opcode = if zc {
                    IORING_OP_SEND_ZC
                } else {
                    IORING_OP_SEND
                };
                s.op_flags = MSG_WAITALL;
                if zc {
                    s.ioprio = IORING_SEND_ZC_REPORT_USAGE;
                }
                if fixed_buf {
                    s.ioprio |= IORING_RECVSEND_FIXED_BUF;
                    s.buf_index = i as u16;
                }
            } else {
                s.opcode = IORING_OP_RECV;
            }
            s
        };

        for i in 0..a.conns {
            ring.push(make(i, &mut bufs))
                .expect("sq sized for all connections");
            if unbatched {
                ring.enter(0).unwrap_or_else(|e| die(&e.to_string()));
            }
        }

        let (mut bytes, mut ops, mut notifs, mut copied) = (0u64, 0u64, 0u64, 0u64);
        let start = Instant::now();
        let cpu0 = thread_cpu_secs();
        let core0 = a.cpu.map(core_busy_and_total);
        let deadline = start + Duration::from_secs(a.secs);
        while Instant::now() < deadline {
            ring.enter(1)
                .unwrap_or_else(|e| die(&format!("io_uring_enter: {e}")));
            while let Some(c) = ring.pop() {
                if c.flags & IORING_CQE_F_NOTIF != 0 {
                    // Second completion of a zero-copy send: buffer released.
                    notifs += 1;
                    if c.res as u32 & IORING_NOTIF_USAGE_ZC_COPIED != 0 {
                        copied += 1;
                    }
                    continue;
                }
                if c.res <= 0 {
                    die(&format!(
                        "{} on connection {} failed: {}",
                        a.cmd,
                        c.user_data,
                        if c.res == 0 {
                            "peer closed".to_string()
                        } else {
                            std::io::Error::from_raw_os_error(-c.res).to_string()
                        }
                    ));
                }
                bytes += c.res as u64;
                ops += 1;
                ring.push(make(c.user_data as usize, &mut bufs))
                    .expect("one op in flight per connection");
                if unbatched {
                    // One completion per syscall: the next `enter(1)` submits this
                    // single SQE and waits again.
                    break;
                }
            }
        }
        let wall = start.elapsed().as_secs_f64();
        let cpu = thread_cpu_secs() - cpu0;
        let gb = bytes as f64 / 1e9;
        // Whole-core utilization of the pinned CPU, including interrupt time.
        let core_util = match (a.cpu, core0) {
            (Some(c), Some((b0, t0))) => {
                let (b1, t1) = core_busy_and_total(c);
                if t1 > t0 {
                    (b1 - b0) as f64 / (t1 - t0) as f64
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        println!(
            "{{\"cmd\":\"{}\",\"mode\":\"{}\",\"arch\":\"{}\",\"conns\":{},\"chunk\":{},\"secs\":{:.2},\"gb\":{:.2},\"gb_per_s\":{:.2},\"thread_cpu\":{:.2},\"core_util\":{:.2},\"gb_per_cpu_s\":{:.2},\"ops\":{},\"enters\":{},\"ops_per_enter\":{:.2},\"zc_notifs\":{},\"zc_copied\":{}}}",
            a.cmd, a.mode, std::env::consts::ARCH, a.conns, a.chunk, wall, gb, gb / wall, cpu / wall, core_util, gb / cpu, ops, ring.enters, ops as f64 / ring.enters as f64, notifs, copied
        );
        // Operations are still in flight and reference `bufs`; exit without unwinding.
        std::process::exit(0);
    }
}
