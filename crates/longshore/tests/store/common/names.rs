// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! A store whose endpoint is a name: the simulated nameserver and a world
//! that routes completions to it.

use super::*;
use longshore::dns::{Family, Resolver};
use std::net::Ipv6Addr;

pub const NS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 53)), 53);

// The shared layout, plus two sockets and three small buffers for the
// resolver and one of each for the nameserver.
pub const WIDE: Limits = Limits {
    sockets_max: LIMITS.sockets_max + 3,
    small_buffers: LIMITS.small_buffers + 4,
    ops_max: 64,
    ..LIMITS
};
pub const NS_SOCK: Sock = Sock(LIMITS.sockets_max + 2);
/// The nameserver's buffer: the small buffer after the resolver's three.
pub fn ns_buf() -> BufId {
    WIDE.small(CONNS + 3)
}
pub const NS_RECV: u64 = 5 << 40;
pub const NS_SEND: u64 = NS_RECV + 1;

/// A nameserver that knows one name and its addresses in each family.
pub struct Nameserver {
    pub a: &'static [Ipv4Addr],
    pub aaaa: &'static [Ipv6Addr],
}

impl Nameserver {
    pub fn recv(io: &mut SimIo) {
        io.submit(
            Token(NS_RECV),
            Op::Recv {
                sock: NS_SOCK,
                buf: ns_buf(),
                off: 0,
                len: 512,
            },
        )
        .unwrap();
    }

    /// Answer the query in the buffer in place. False if the token is not ours.
    pub fn on(&self, io: &mut SimIo, token: u64, result: i32) -> bool {
        match token {
            NS_SEND => Self::recv(io),
            NS_RECV => {
                let n = result as usize;
                let m = io.buf(ns_buf());
                let rtype = m[n - 3];
                let aaaa = rtype == 28;
                (m[2], m[3]) = (0x81, 0x80);
                let mut end = n;
                let mut record = |data: &[u8]| {
                    m[end..end + 12].copy_from_slice(&[
                        0xc0,
                        12,
                        0,
                        rtype,
                        0,
                        1,
                        0,
                        0,
                        0,
                        60,
                        0,
                        data.len() as u8,
                    ]);
                    m[end + 12..end + 12 + data.len()].copy_from_slice(data);
                    end += 12 + data.len();
                };
                let mut count = 0;
                if aaaa {
                    for a in self.aaaa {
                        record(&a.octets());
                        count += 1;
                    }
                } else {
                    for a in self.a {
                        record(&a.octets());
                        count += 1;
                    }
                }
                m[7] = count;
                io.submit(
                    Token(NS_SEND),
                    Op::Send {
                        sock: NS_SOCK,
                        buf: ns_buf(),
                        off: 0,
                        len: end as u32,
                        zero_copy: false,
                    },
                )
                .unwrap();
            }
            _ => return false,
        }
        true
    }
}

/// A store whose endpoint is a name, an object server listening at
/// `listen`, and the nameserver.
pub fn resolved_world(
    family: Family,
    prefer_large_segments: Option<usize>,
    listen: SocketAddr,
    ns: Nameserver,
) -> (World, Nameserver) {
    let mut io = SimIo::new(WIDE, 7, Faults::default());
    let mut server = Server::new(server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 0,
        head_max: 2048,
        buffer_size: BUF,
        objects_max: 8,
        token_base: 2 << 40,
        checksum: None,
    });
    server.start(&mut io, listen).unwrap();
    io.bind_udp(NS_SOCK, NS);
    Nameserver::recv(&mut io);
    let mut cfg = s3_config(3, false);
    cfg.endpoint = Endpoint::Resolved(Resolver::new(longshore::dns::Config {
        servers: vec![NS],
        hosts: vec![cfg.host.clone()],
        family,
        prefer_large_segments,
        refresh_ns: 30_000 * MS,
        timeout_ns: 200 * MS,
        socks: [Sock(LIMITS.sockets_max), Sock(LIMITS.sockets_max + 1)],
        rx_bufs: [WIDE.small(CONNS), WIDE.small(CONNS + 1)],
        tx_buf: WIDE.small(CONNS + 2),
        token_base: 3 << 40,
    }));
    let mut s3 = S3::new(cfg);
    s3.start(&mut io);
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    let world = World {
        io,
        s3,
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(8),
    };
    (world, ns)
}

/// Store one object and return how the operation ended.
pub fn store(w: &mut World, ns: &Nameserver, token: u64, key: &str) -> Done {
    w.fill_tx(100, token as u8);
    w.s3.store(&mut w.io, token, key, TX, 100, Checksum::None)
        .unwrap();
    next_done(w, ns)
}

/// Run until an operation finishes, and return how it ended.
pub fn next_done(w: &mut World, ns: &Nameserver) -> Done {
    let World {
        io,
        s3,
        server,
        out,
        ..
    } = w;
    sealed(|| loop {
        if let Some(d) = s3.poll() {
            return d;
        }
        io.tick(true, out).expect("simulation deadlock");
        while let Some(c) = out.pop() {
            if c.token.0 == HEARTBEAT {
                s3.tick(io);
                io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                    .unwrap();
            } else if !s3.on_completion(io, c) && !ns.on(io, c.token.0, c.result) {
                assert!(server.on_completion(io, c), "unrouted completion {c:?}");
            }
        }
    })
}
