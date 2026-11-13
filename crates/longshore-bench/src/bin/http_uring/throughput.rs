// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Throughput benchmarks: PUT and GET against the sink.

use super::*;

pub(super) fn bench(a: &Args, put: bool) {
    if let Some(cpu) = a.cpu {
        let mut mask = [0u64; 16];
        mask[cpu / 64] |= 1 << (cpu % 64);
        if unsafe { sched_setaffinity(0, core::mem::size_of_val(&mask), mask.as_ptr()) } != 0 {
            die(&format!("cannot pin to cpu {cpu}"));
        }
    }
    // Several addresses, separated by commas, spread the connections over
    // several links: connection i uses address i mod the number given.
    let addrs: Vec<SocketAddr> = a
        .addr
        .split(',')
        .map(|s| {
            s.parse()
                .unwrap_or_else(|_| die("--addr must be IPv4:port[,IPv4:port...]"))
        })
        .collect();
    let crc = a.crc;
    let n = a.conns;
    // Large buffers: one body buffer per connection, then two record
    // buffers per connection for TLS. Small buffers: one head per connection.
    let limits = Limits {
        sockets_max: n,
        buffers: n * 3,
        buffer_size: a.size.max(256 * 1024),
        small_buffers: n,
        small_buffer_size: 4096,
        ops_max: n * 3 + 1,
    };
    let mut io = UringIo::new(limits).unwrap_or_else(|e| die(&e.to_string()));
    let mut cfg = client_cfg(n, limits.buffer_size, 0);
    cfg.first_buf = limits.small(0).0;
    cfg.tls_batch = a.batch;
    let host = if a.tls_cert.is_some() {
        "bench.test"
    } else {
        "bench"
    };
    let mut client = match &a.tls_cert {
        None => Client::new(cfg),
        Some(path) => {
            let der = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
            Client::with_tls(
                cfg,
                longshore::tls::handshake::ClientSetup::with_roots(&[&der])
                    .unwrap_or_else(|e| die(&format!("certificate: {e:?}"))),
                n,
            )
        }
    };
    let mut out = Completions::new(64);
    let size = a.size;
    let start_one = |client: &mut Client, io: &mut UringIo, i: u32| {
        let body = BufId(i);
        let link = i as usize % addrs.len();
        let (addr, host_id) = (addrs[link], link as u16);
        // With --crc every PUT is checksummed the way a store would be asked
        // to verify it: CRC-64/NVME over the whole body, each time.
        let sum;
        let checksum: [(&str, &[u8]); 1];
        let headers: &[(&str, &[u8])] = if put && crc {
            sum = longshore::checksum::crc64_header_value(longshore::checksum::crc64_nvme(
                &io.buf(body)[..size as usize],
            ));
            checksum = [("x-amz-checksum-crc64nvme", &sum)];
            &checksum
        } else {
            &[]
        };
        let req = if put {
            Request {
                method: "PUT",
                target: "/bench/object",
                host,
                host_id,
                addr,
                headers,
                body: Some((body, size)),
                response_buf: None,
            }
        } else {
            Request {
                method: "GET",
                target: "/bench/object",
                host,
                host_id,
                addr,
                headers,
                body: None,
                response_buf: Some(body),
            }
        };
        client.start(io, i as u64, &req)
    };
    for i in 0..n {
        start_one(&mut client, &mut io, i).unwrap_or_else(|e| die(&format!("start: {e:?}")));
    }
    // A peer that closes after a response leaves its slot Closing until the
    // close completes, so a restart can see Busy; retry it after the next tick.
    let mut waiting = vec![false; n as usize];
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
        .unwrap();

    let deadline = io.now_ns() + a.secs * 1000 * MS;
    let (t0, cpu0, core0, enters0) = (
        io.now_ns(),
        thread_cpu_secs(),
        a.cpu.map(core_busy_and_total),
        io.enters(),
    );
    let (bytes, requests) = sealed(|| {
        let (mut bytes, mut requests) = (0u64, 0u64);
        // Formatting would allocate; report start errors other than Busy by exit code.
        let restart =
            |client: &mut Client, io: &mut UringIo, i: u32, waiting: &mut [bool]| match start_one(
                client, io, i,
            ) {
                Ok(()) => waiting[i as usize] = false,
                Err(Error::Busy) => waiting[i as usize] = true,
                Err(_) => std::process::exit(3),
            };
        while io.now_ns() < deadline {
            io.tick(true, &mut out).expect("tick");
            while let Some(c) = out.pop() {
                if c.token.0 == HEARTBEAT {
                    client.tick(&mut io);
                    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 100 * MS })
                        .unwrap();
                    continue;
                }
                if let Handled::Done(d) = client.on_completion(&mut io, c) {
                    match d.result {
                        Ok(r) if r.status == 200 => {
                            bytes += if put { size as u64 } else { r.body_len as u64 };
                            requests += 1;
                            restart(&mut client, &mut io, d.token as u32, &mut waiting);
                        }
                        // Formatting would allocate; report by exit code.
                        _ => std::process::exit(3),
                    }
                }
            }
            for i in 0..n {
                if waiting[i as usize] {
                    restart(&mut client, &mut io, i, &mut waiting);
                }
            }
        }
        (bytes, requests)
    });
    let wall = (io.now_ns() - t0) as f64 / 1e9;
    let cpu = thread_cpu_secs() - cpu0;
    let core = match (a.cpu, core0) {
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
    let gb = bytes as f64 / 1e9;
    println!(
        "{{\"cmd\":\"{}\",\"crc\":{crc},\"links\":{},\"tls\":{},\"batch\":{},\"arch\":\"{}\",\"conns\":{},\"size\":{},\"secs\":{:.2},\"requests\":{},\"gb_per_s\":{:.2},\"thread_cpu\":{:.2},\"core_util\":{:.2},\"gb_per_cpu_s\":{:.2},\"enters_per_request\":{:.1},\"connects\":{}}}",
        a.cmd, addrs.len(), a.tls_cert.is_some(), a.batch, std::env::consts::ARCH, n, size, wall, requests, gb / wall, cpu / wall, core, gb / cpu, (io.enters() - enters0) as f64 / requests.max(1) as f64, client.connects
    );
    std::process::exit(0);
}
