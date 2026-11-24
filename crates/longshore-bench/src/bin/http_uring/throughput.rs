// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Throughput benchmarks: PUT and GET against the sink.

use super::*;
use crate::die::OrDie;

/// Pin the calling thread to `cpu`, if one is given.
fn pin(cpu: Option<usize>) {
    if let Some(cpu) = cpu {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        if unsafe { sched_setaffinity(0, core::mem::size_of_val(&mask), mask.as_ptr()) } != 0 {
            die(format_args!("cannot pin to cpu {cpu}"));
        }
    }
}

/// Several addresses, separated by commas, spread the connections over
/// several links: connection i uses address i mod the number given.
fn addrs(list: &str) -> Vec<SocketAddr> {
    list.split(',')
        .map(|s| {
            s.parse()
                .unwrap_or_else(|_| die("--addr must be IPv4:port[,IPv4:port...]"))
        })
        .collect()
}

/// The client: plain, or with --tls-cert speaking TLS to a server that
/// presents the certificate in that file.
fn client(a: &Args, cfg: client::ClientConfig) -> Client {
    match &a.trust_file {
        None => Client::new(cfg),
        Some(path) => {
            let der = std::fs::read(path).unwrap_or_else(|e| die(format_args!("{path}: {e}")));
            Client::with_tls(
                cfg,
                longshore::tls::handshake::ClientSetup::with_roots_using(a.provider, &[&der])
                    .unwrap_or_else(|e| die(format_args!("certificate: {e:?}"))),
                a.conns,
            )
        }
    }
}

/// The share of `cpu` that was busy since `core0` was read, or 0.
fn core_util(cpu: Option<usize>, core0: Option<(u64, u64)>) -> f64 {
    match (cpu, core0) {
        (Some(c), Some((b0, total0))) => {
            let (b1, total1) = core_busy_and_total(c);
            if total1 > total0 {
                (b1 - b0) as f64 / (total1 - total0) as f64
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// What every request of the run is: PUT or GET of `size` bytes, to which
/// address and host, and whether the body is checksummed.
struct Load<'a> {
    addrs: &'a [SocketAddr],
    host: &'a str,
    put: bool,
    crc: bool,
    size: u32,
    conns: u32,
}

impl Load<'_> {
    /// Start the request on connection `i`, with body or response buffer `i`.
    fn start(&self, http: &mut Client, ring: &mut UringIo, i: u32) -> Result<(), Error> {
        let (put, size, host) = (self.put, self.size, self.host);
        let body = BufId::new(i);
        let link = i as usize % self.addrs.len();
        let (addr, host_id) = (self.addrs[link], link as u16);
        // With --crc every PUT is checksummed the way a store would be asked
        // to verify it: CRC-64/NVME over the whole body, each time.
        let sum;
        let checksum: [(&str, &[u8]); 1];
        let headers: &[(&str, &[u8])] = if put && self.crc {
            sum = longshore::checksum::crc64_header_value(longshore::checksum::crc64_nvme(
                &ring.buf(body)[..size as usize],
            ));
            checksum = [("x-amz-checksum-crc64nvme", &sum)];
            &checksum
        } else {
            &[]
        };
        let req = if put {
            Request::new("PUT", "/bench/object", host, host_id, addr)
                .with_headers(headers)
                .with_body(Some((body, size)))
        } else {
            Request::new("GET", "/bench/object", host, host_id, addr)
                .with_headers(headers)
                .with_response_buf(Some(body))
        };
        http.start(ring, u64::from(i), &req)
    }

    /// Start the next request on connection `i`, or mark it for a retry
    /// after the next tick if its slot is still busy. Formatting would
    /// allocate; report start errors other than Busy by exit code.
    fn restart(&self, http: &mut Client, ring: &mut UringIo, i: u32, retry: &mut [bool]) {
        match self.start(http, ring, i) {
            Ok(()) => retry[i as usize] = false,
            Err(Error::Busy) => retry[i as usize] = true,
            Err(_) => std::process::exit(3),
        }
    }
}

/// The measured window: restart each request as it finishes until
/// `deadline`. Returns the bytes and requests it saw.
fn measure(
    io: &mut UringIo,
    client: &mut Client,
    out: &mut Completions,
    load: &Load,
    deadline: u64,
    waiting: &mut [bool],
) -> (u64, u64) {
    let (put, size, n) = (load.put, load.size, load.conns);
    sealed(|| {
        let (mut bytes, mut requests) = (0u64, 0u64);
        while io.now_ns() < deadline {
            io.tick(true, out).or_die("tick");
            while let Some(c) = out.pop() {
                if c.token().get() == HEARTBEAT {
                    client.tick(io);
                    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
                        .or_die("submit");
                    continue;
                }
                if let Handled::Done(d) = client.on_completion(io, c) {
                    match d.result() {
                        Ok(r) if r.status() == 200 => {
                            bytes += if put {
                                u64::from(size)
                            } else {
                                u64::from(r.body_len())
                            };
                            requests += 1;
                            load.restart(client, io, d.token() as u32, waiting);
                        }
                        // Formatting would allocate; report by exit code.
                        _ => std::process::exit(3),
                    }
                }
            }
            for i in 0..n {
                if waiting[i as usize] {
                    load.restart(client, io, i, waiting);
                }
            }
        }
        (bytes, requests)
    })
}

pub(super) fn bench(a: &Args, put: bool) {
    pin(a.cpu);
    let addrs = addrs(&a.addr);
    let crc = a.crc;
    let n = a.conns;
    // Large buffers: one body buffer per connection, then two record
    // buffers per connection for TLS. Small buffers: one head per connection.
    let limits = Limits::BASE
        .with_sockets_max(n)
        .with_buffers(n * 3)
        .with_buffer_size(a.size.max(256 * 1024))
        .with_small_buffers(n)
        .with_small_buffer_size(4096)
        .with_ops_max(n * 3 + 1);
    let mut io = UringIo::new(limits).unwrap_or_else(|e| die(e));
    let cfg = client_cfg(n, limits.buffer_size(), 0, limits.small(0).get())
        .tls_batch(a.batch)
        .build()
        .or_die("the client configuration");
    let host = if a.trust_file.is_some() {
        "bench.test"
    } else {
        "bench"
    };
    let mut client = client(a, cfg);
    let mut out = Completions::new(64);
    let size = a.size;
    let load = Load {
        addrs: &addrs,
        host,
        put,
        crc,
        size,
        conns: n,
    };
    for i in 0..n {
        load.start(&mut client, &mut io, i)
            .unwrap_or_else(|e| die(format_args!("start: {e:?}")));
    }
    // A peer that closes after a response leaves its slot Closing until the
    // close completes, so a restart can see Busy; retry it after the next tick.
    let mut waiting = vec![false; n as usize];
    io.submit(Token::new(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
        .or_die("submit");

    let deadline = io.now_ns() + a.secs * 1000 * MS;
    let (t0, cpu0, core0, enters0) = (
        io.now_ns(),
        thread_cpu_secs(),
        a.cpu.map(core_busy_and_total),
        io.enters(),
    );
    let (bytes, requests) = measure(
        &mut io,
        &mut client,
        &mut out,
        &load,
        deadline,
        &mut waiting,
    );
    let wall = (io.now_ns() - t0) as f64 / 1e9;
    let cpu = thread_cpu_secs() - cpu0;
    let core = core_util(a.cpu, core0);
    let gb = bytes as f64 / 1e9;
    println!(
        "{{\"cmd\":\"{}\",\"crc\":{crc},\"links\":{},\"tls\":{},\"batch\":{},\"arch\":\"{}\",\"conns\":{},\"size\":{},\"secs\":{:.2},\"requests\":{},\"gb_per_s\":{:.2},\"thread_cpu\":{:.2},\"core_util\":{:.2},\"gb_per_cpu_s\":{:.2},\"enters_per_request\":{:.1},\"connects\":{}}}",
        a.cmd, addrs.len(), a.trust_file.is_some(), a.batch, std::env::consts::ARCH, n, size, wall, requests, gb / wall, cpu / wall, core, gb / cpu, (io.enters() - enters0) as f64 / requests.max(1) as f64, client.connects()
    );
    std::process::exit(0);
}
