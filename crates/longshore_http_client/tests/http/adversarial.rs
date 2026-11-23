// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 5 adversarial HTTP/HTTPS client tests (`GAP-H1`).

mod common;
use common::*;

#[test]
fn cancel_in_closing_then_connect_and_host_len_over_253() {
    let mut w = world(99, Faults::default());
    let too_long_host = "a".repeat(254);
    let bad_req = Request {
        method: "GET",
        target: "/k",
        host: &too_long_host,
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: None,
    };
    assert_eq!(
        w.client.start(&mut w.io, 1, &bad_req),
        Err(Error::BadRequest)
    );

    let mut w1 = world_with(
        99,
        Faults::default(),
        client::Config {
            connections_max: 1,
            ..client_cfg()
        },
    );
    ok(404, 0)(w1.request("GET", "/missing", None));
    let req2 = Request {
        method: "GET",
        target: "/k",
        host: "other.test",
        host_id: 1,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    w1.client.start(&mut w1.io, 777, &req2).unwrap();
    let cancelled = w1.client.cancel(&mut w1.io, 777);
    assert_eq!(
        cancelled,
        Handled::Done(Done {
            token: 777,
            result: Err(Error::Timeout),
            sent: false,
        })
    );
    w1.settle(20);
    assert_eq!(w1.client.in_flight(), 0);
    assert_eq!(
        w1.client.connects, 1,
        "cancelled Closing{{then_connect:true}} must not dial host 1"
    );
}

#[test]
fn head_len_equals_head_max_skips_idle_check_on_reuse() {
    // Request head of exact size head_max skips check_idle=true and reuses connection cleanly.
    let small_head_cfg = client::Config {
        head_max: 128,
        check_idle: true,
        ..client_cfg()
    };
    let mut w = world_with(101, Faults::default(), small_head_cfg);
    ok(404, 0)(w.request("GET", "/a", None));

    // Measure exact request size for a minimal GET and pad target to hit head_max == 128.
    let base_len = b"GET / HTTP/1.1\r\nHost: objects.test\r\n\r\n".len();
    let pad = 128 - base_len;
    let target = format!("/{}", "x".repeat(pad));
    let req = Request {
        method: "GET",
        target: &target,
        host: "objects.test",
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    w.client.start(&mut w.io, 888, &req).unwrap();
    w.run_until_done(2);
    assert_eq!(w.done[1].result.unwrap().status, 404);
    assert_eq!(w.client.connects, 1);
}

#[cfg(feature = "tls")]
#[test]
fn cancel_during_tls_handshake_and_invalid_tls_hostname() {
    use longshore_stream::TlsError;
    use longshore_tls::handshake::{ClientSetup, ServerSetup};

    let k = rcgen::generate_simple_self_signed(vec!["objects.test".to_string()]).unwrap();
    let cert = k.cert.der().to_vec();
    let key = k.signing_key.serialize_der();
    let cs = ClientSetup::with_roots(&[&cert]).unwrap();
    let ss = ServerSetup::new(&[&cert], &key).unwrap();

    const TLS_BUF: u32 = 64 * 1024;
    let tls_limits = Limits {
        sockets_max: 1 + CONNS * 2,
        buffers: 16,
        buffer_size: TLS_BUF,
        ops_max: CONNS * 3 + CONNS + 2,
        ..Limits::BASE
    };
    let tls_cli_cfg = client::Config {
        buffer_size: TLS_BUF,
        ..client_cfg()
    };
    let tls_srv_cfg = server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 6,
        head_max: HEAD_MAX,
        buffer_size: TLS_BUF,
        objects_max: 4,
        token_base: 2 << 40,
        checksum: None,
    };

    // Invalid hostname triggers TlsError::Setup during connect -> handshake transition.
    let mut io = SimIo::new(tls_limits, 55, Faults::default());
    let mut srv = Server::with_tls(tls_srv_cfg, ss, 10);
    srv.start(&mut io, ADDR).unwrap();
    let mut cli = Client::with_tls(tls_cli_cfg, cs, 2);
    let bad = Request {
        method: "GET",
        target: "/k",
        host: "not a host:443",
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    cli.start(&mut io, 900, &bad).unwrap();
    let mut out = Completions::new(16);
    let mut res = None;
    while res.is_none() {
        io.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            match cli.on_completion(&mut io, c) {
                Handled::Done(d) => res = Some(d.result),
                Handled::Pending => {}
                Handled::NotMine => {
                    let _: bool = srv.on_completion(&mut io, c);
                }
            }
        }
    }
    assert_eq!(res.unwrap(), Err(Error::Tls(TlsError::Setup)));

    // Explicit Client::cancel() mid-TlsHandshake (plain server stalls in RecvHead after ClientHello).
    let mut io2 = SimIo::new(tls_limits, 56, Faults::default());
    let mut plain_srv = Server::new(tls_srv_cfg);
    plain_srv.start(&mut io2, ADDR).unwrap();
    let cs2 = ClientSetup::with_roots(&[&cert]).unwrap();
    let mut cli2 = Client::with_tls(tls_cli_cfg, cs2, 2);
    let req = Request {
        method: "GET",
        target: "/k",
        host: "objects.test",
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    cli2.start(&mut io2, 901, &req).unwrap();
    for _ in 0..5 {
        io2.tick(false, &mut out).unwrap();
        while let Some(c) = out.pop() {
            if cli2.on_completion(&mut io2, c) == Handled::NotMine {
                let _: bool = plain_srv.on_completion(&mut io2, c);
            }
        }
    }
    assert_eq!(cli2.cancel(&mut io2, 901), Handled::Pending);
    let mut done2 = None;
    while done2.is_none() {
        io2.tick(true, &mut out).unwrap();
        while let Some(c) = out.pop() {
            match cli2.on_completion(&mut io2, c) {
                Handled::Done(d) => done2 = Some(d.result),
                Handled::Pending => {}
                Handled::NotMine => {
                    let _: bool = plain_srv.on_completion(&mut io2, c);
                }
            }
        }
    }
    assert_eq!(done2.unwrap(), Err(Error::Timeout));
    assert_eq!(cli2.in_flight(), 0);
}
