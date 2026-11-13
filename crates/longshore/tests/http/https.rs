// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTPS: the client and the in-tree server over TLS on one simulated `Io`.
//! rustls performs each handshake; every record after it is sealed and
//! opened by `longshore::tls`. Allocation is forbidden throughout, except inside
//! the handshake's own scoped exception.

use longshore::http::client::{self, Client, Done, Error, Handled, Request, Response};
use longshore::http::server::{self, Behavior, Server};
use longshore::io::alloc_guard::{sealed, Guard};
use longshore::io::{BufId, Completions, Io, Limits, Op, Sock, Token};
use longshore::sim::{Faults, SimIo};
use longshore::tls::handshake::{ClientSetup, ServerSetup, TlsError};
use std::net::{Ipv4Addr, SocketAddrV4};

#[global_allocator]
static ALLOC: Guard = Guard;

const CONNS: u32 = 2;
/// Pool buffers must hold two TLS records.
const BUF: u32 = 64 * 1024;
const HEAD_MAX: u32 = 2048;
const MS: u64 = 1_000_000;
const ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 80), 443);
const HEARTBEAT: u64 = 1 << 50;
const NAME: &str = "objects.test";

// Buffers: client heads 0..2, client TLS 2..6, server (head, body) 6..10,
// server TLS 10..14, user tx 14, rx 15.
const TX: BufId = BufId(14);
const RX: BufId = BufId(15);
const LIMITS: Limits = Limits {
    sockets_max: 1 + CONNS * 2,
    buffers: 16,
    buffer_size: BUF,
    small_buffers: 0,
    small_buffer_size: 0,
    ops_max: CONNS * 3 + CONNS + 1 + 1,
};

struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

fn identity(name: &str) -> Identity {
    let k = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    Identity {
        cert: k.cert.der().to_vec(),
        key: k.signing_key.serialize_der(),
    }
}

struct World {
    io: SimIo,
    client: Client,
    server: Server,
    out: Completions,
    done: Vec<Done>,
}

fn client_cfg() -> client::Config {
    client::Config {
        connections_max: CONNS,
        first_sock: 1,
        first_buf: 0,
        head_max: HEAD_MAX,
        buffer_size: BUF,
        max_age_ns: 60_000 * MS,
        max_requests: 1000,
        request_timeout_ns: 2_000 * MS,
        zero_copy_min: 4096,
        token_base: 1 << 40,
        tls_batch: 256 * 1024,
        tls_records_max: client::TLS_RECORDS_MAX,
        check_idle: true,
    }
}

fn server_cfg() -> server::Config {
    server::Config {
        listener: Sock(0),
        connections_max: CONNS,
        first_sock: 1 + CONNS,
        first_buf: 6,
        head_max: HEAD_MAX,
        buffer_size: BUF,
        objects_max: 2,
        token_base: 2 << 40,
        checksum: None,
    }
}

/// `client_tls` and `server_tls` choose each side's protocol independently.
fn world_with(
    seed: u64,
    faults: Faults,
    client_tls: Option<ClientSetup>,
    server_tls: Option<ServerSetup>,
) -> World {
    world_cfg(seed, faults, client_cfg(), client_tls, server_tls)
}

fn world_cfg(
    seed: u64,
    faults: Faults,
    cfg: client::Config,
    client_tls: Option<ClientSetup>,
    server_tls: Option<ServerSetup>,
) -> World {
    let mut io = SimIo::new(LIMITS, seed, faults);
    let mut server = match server_tls {
        Some(s) => Server::with_tls(server_cfg(), s, 10),
        None => Server::new(server_cfg()),
    };
    server.start(&mut io, ADDR).unwrap();
    let client = match client_tls {
        Some(s) => Client::with_tls(cfg, s, 2),
        None => Client::new(cfg),
    };
    io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
        .unwrap();
    World {
        io,
        client,
        server,
        out: Completions::new(16),
        done: Vec::with_capacity(64),
    }
}

fn world(seed: u64, faults: Faults) -> World {
    let id = identity(NAME);
    world_with(
        seed,
        faults,
        Some(ClientSetup::with_roots(&[&id.cert]).unwrap()),
        Some(ServerSetup::new(&[&id.cert], &id.key).unwrap()),
    )
}

impl World {
    fn run_until_done(&mut self, n: usize) {
        let World {
            io,
            client,
            server,
            out,
            done,
        } = self;
        sealed(|| {
            while done.len() < n {
                io.tick(true, out).expect("simulation deadlock");
                while let Some(c) = out.pop() {
                    if c.token.0 == HEARTBEAT {
                        client.tick(io);
                        io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                            .unwrap();
                        continue;
                    }
                    match client.on_completion(io, c) {
                        Handled::Done(d) => done.push(d),
                        Handled::Pending => {}
                        Handled::NotMine => {
                            assert!(server.on_completion(io, c), "unrouted completion {c:?}")
                        }
                    }
                }
            }
        })
    }

    fn request(
        &mut self,
        method: &str,
        target: &str,
        body: Option<u32>,
    ) -> Result<Response, Error> {
        let token = self.done.len() as u64 + 100;
        let req = Request {
            method,
            target,
            host: NAME,
            host_id: 0,
            addr: ADDR,
            headers: &[("x-test", b"1")],
            body: body.map(|n| (TX, n)),
            response_buf: Some(RX),
        };
        self.client
            .start(&mut self.io, token, &req)
            .expect("a connection should be free");
        self.run_until_done(self.done.len() + 1);
        let d = *self.done.last().unwrap();
        assert_eq!(d.token, token);
        d.result
    }

    fn fill_tx(&mut self, len: u32, salt: u8) {
        for (i, b) in self.io.buf(TX)[..len as usize].iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ salt;
        }
    }
}

fn ok(status: u16, body_len: u32) -> impl Fn(Result<Response, Error>) {
    move |r| match r {
        Ok(resp) => assert_eq!((resp.status, resp.body_len), (status, body_len)),
        Err(e) => panic!("expected {status}, got {e:?}"),
    }
}

#[test]
fn put_then_get_round_trips_over_tls_on_one_connection() {
    let mut w = world(1, Faults::default());
    // Sizes around record boundaries: empty, one byte, exactly one record,
    // one byte more, and several records.
    for (i, len) in [0u32, 1, 16_384, 16_385, 60_000].into_iter().enumerate() {
        w.fill_tx(len, i as u8);
        ok(200, 0)(w.request("PUT", "/k", Some(len)));
        assert_eq!(w.server.object_len("/k"), Some(len));
        let sent = w.io.buf(TX)[..len as usize].to_vec();
        assert_eq!(
            w.server.object("/k"),
            Some(&sent[..]),
            "{len} bytes: server stored something else"
        );
        ok(200, len)(w.request("GET", "/k", None));
        assert!(
            w.io.buf(RX)[..len as usize] == sent[..],
            "{len} bytes: fetched bytes differ"
        );
    }
    ok(200, 0)(w.request("HEAD", "/k", None));
    ok(404, 0)(w.request("GET", "/missing", None));
    assert_eq!(
        (w.client.connects, w.server.accepted),
        (1, 1),
        "one handshake served every request"
    );
}

/// Records split across reads, sends accepted in pieces, completions
/// delayed: the plaintext is unchanged.
#[test]
fn tls_survives_short_writes_partial_reads_and_delays() {
    let faults = Faults {
        short_send: 300_000,
        partial_recv: 300_000,
        delay: 200_000,
        ..Faults::default()
    };
    for seed in 0..25 {
        let mut w = world(seed, faults);
        w.fill_tx(50_000, seed as u8);
        ok(200, 0)(w.request("PUT", "/k", Some(50_000)));
        ok(200, 50_000)(w.request("GET", "/k", None));
        let sent = w.io.buf(TX)[..50_000].to_vec();
        assert!(
            w.io.buf(RX)[..50_000] == sent[..],
            "seed {seed}: fetched bytes differ"
        );
        assert_eq!(
            w.server.object("/k"),
            Some(&sent[..]),
            "seed {seed}: stored bytes differ"
        );
    }
}

/// TLS authenticates every record, so a flipped bit can never reach the
/// application: a request either succeeds with exact bytes or fails.
#[test]
fn a_flipped_bit_fails_the_request_and_never_corrupts_data() {
    let faults = Faults {
        bit_flip: 20_000,
        short_send: 100_000,
        partial_recv: 100_000,
        ..Faults::default()
    };
    let (mut clean, mut failed) = (0, 0);
    for seed in 0..60 {
        let mut w = world(seed, faults);
        w.fill_tx(40_000, seed as u8);
        let sent = w.io.buf(TX)[..40_000].to_vec();
        match w.request("PUT", "/k", Some(40_000)) {
            Ok(r) => {
                assert_eq!(r.status, 200, "seed {seed}");
                assert_eq!(
                    w.server.object("/k"),
                    Some(&sent[..]),
                    "seed {seed}: PUT succeeded with damaged bytes"
                );
            }
            Err(Error::Tls(_) | Error::Io(_) | Error::Closed { .. } | Error::Timeout) => {
                failed += 1;
                // Whatever the server holds, it is not a damaged version.
                assert!(
                    w.server.object("/k").is_none_or(|o| o == &sent[..]),
                    "seed {seed}: server stored damaged bytes"
                );
                continue;
            }
            Err(e) => panic!("seed {seed}: unexpected {e:?}"),
        }
        match w.request("GET", "/k", None) {
            Ok(r) => {
                assert_eq!((r.status, r.body_len), (200, 40_000), "seed {seed}");
                assert!(
                    w.io.buf(RX)[..40_000] == sent[..],
                    "seed {seed}: GET succeeded with damaged bytes"
                );
                clean += 1;
            }
            Err(Error::Tls(_) | Error::Io(_) | Error::Closed { .. } | Error::Timeout) => {
                failed += 1
            }
            Err(e) => panic!("seed {seed}: unexpected {e:?}"),
        }
    }
    assert!(
        clean >= 10 && failed >= 10,
        "expected both outcomes, got {clean} clean and {failed} failed"
    );
}

#[test]
fn an_untrusted_or_misnamed_certificate_fails_before_any_request_is_sent() {
    let (server_id, other) = (identity(NAME), identity(NAME));
    // The client trusts a different certificate.
    let mut w = world_with(
        2,
        Faults::default(),
        Some(ClientSetup::with_roots(&[&other.cert]).unwrap()),
        Some(ServerSetup::new(&[&server_id.cert], &server_id.key).unwrap()),
    );
    assert_eq!(
        w.request("GET", "/k", None),
        Err(Error::Tls(TlsError::BadCertificate))
    );
    assert_eq!(
        w.server.requests, 0,
        "no request may reach a server the client does not trust"
    );
    // The certificate is trusted but is for another name.
    let wrong = identity("elsewhere.test");
    let mut w = world_with(
        3,
        Faults::default(),
        Some(ClientSetup::with_roots(&[&wrong.cert]).unwrap()),
        Some(ServerSetup::new(&[&wrong.cert], &wrong.key).unwrap()),
    );
    assert_eq!(
        w.request("GET", "/k", None),
        Err(Error::Tls(TlsError::BadCertificate))
    );
    assert_eq!((w.server.requests, w.client.in_flight()), (0, 0));
}

#[test]
fn protocol_mismatches_fail_cleanly() {
    let id = identity(NAME);
    // TLS client, plain server. This server keeps waiting for a request head
    // that never comes, so the client's timeout ends it; a server that
    // answered in HTTP would be rejected as a bad record instead.
    let mut w = world_with(
        4,
        Faults::default(),
        Some(ClientSetup::with_roots(&[&id.cert]).unwrap()),
        None,
    );
    assert!(
        matches!(
            w.request("GET", "/k", None),
            Err(Error::Timeout | Error::Tls(TlsError::BadRecord))
        ),
        "TLS to a plain port must fail, not hang"
    );
    assert_eq!(w.client.in_flight(), 0);
    // Plain client, TLS server: the server drops what is not a handshake.
    let mut w = world_with(
        5,
        Faults::default(),
        None,
        Some(ServerSetup::new(&[&id.cert], &id.key).unwrap()),
    );
    assert!(
        matches!(
            w.request("GET", "/k", None),
            Err(Error::Closed { .. } | Error::Io(_))
        ),
        "plain request to a TLS port must fail, not hang"
    );
    assert_eq!(w.client.in_flight(), 0);
}

#[test]
fn timeouts_and_server_failures_work_the_same_over_tls() {
    let mut w = world(6, Faults::default());
    w.fill_tx(1000, 0);
    ok(200, 0)(w.request("PUT", "/k", Some(1000)));
    w.server.script(Behavior::Stall);
    assert_eq!(w.request("GET", "/k", None), Err(Error::Timeout));
    w.server.script(Behavior::TruncateBody);
    assert!(matches!(
        w.request("GET", "/k", None),
        Err(Error::Closed { .. })
    ));
    w.server.script(Behavior::Status(503));
    ok(503, 0)(w.request("GET", "/k", None));
    ok(200, 1000)(w.request("GET", "/k", None));
    assert_eq!(w.client.in_flight(), 0);
    assert_eq!(
        w.client.connects, 3,
        "a new handshake after each of the two failures"
    );
}

#[test]
fn chunked_responses_work_over_tls() {
    let mut w = world(
        7,
        Faults {
            partial_recv: 300_000,
            short_send: 300_000,
            ..Faults::default()
        },
    );
    w.fill_tx(30_000, 4);
    ok(200, 0)(w.request("PUT", "/k", Some(30_000)));
    let sent = w.io.buf(TX)[..30_000].to_vec();
    for chunk in [500u32, 16_384, 30_000] {
        w.server.script(Behavior::ChunkedBody(chunk));
        ok(200, 30_000)(w.request("GET", "/k", None));
        assert!(w.io.buf(RX)[..30_000] == sent[..], "chunk size {chunk}");
    }
    assert_eq!(w.client.connects, 1);
}

/// Traffic keys are never used past the configured number of records: a
/// connection that reaches it finishes its request, is closed, and the next
/// request handshakes again. No request fails and no byte changes.
#[test]
fn a_connection_is_replaced_before_its_keys_wear_out() {
    let id = identity(NAME);
    let cfg = client::Config {
        tls_records_max: 12,
        ..client_cfg()
    };
    let mut w = world_cfg(
        21,
        Faults::default(),
        cfg,
        Some(ClientSetup::with_roots(&[&id.cert]).unwrap()),
        Some(ServerSetup::new(&[&id.cert], &id.key).unwrap()),
    );
    // 60 000 bytes is four records each way, so the limit is reached every
    // second or third request.
    for i in 0..12u8 {
        w.fill_tx(60_000, i);
        ok(200, 0)(w.request("PUT", "/k", Some(60_000)));
        ok(200, 60_000)(w.request("GET", "/k", None));
        let sent = w.io.buf(TX)[..60_000].to_vec();
        assert!(
            w.io.buf(RX)[..60_000] == sent[..],
            "round {i}: fetched bytes differ"
        );
    }
    assert!(
        (4..=24).contains(&w.client.connects),
        "24 requests of about 5 records against a limit of 12 need several connections, not {}",
        w.client.connects
    );
    assert_eq!(w.client.connects, w.server.accepted);

    // With the default limit the same work stays on one connection.
    let mut w = world(21, Faults::default());
    for i in 0..12u8 {
        w.fill_tx(60_000, i);
        ok(200, 0)(w.request("PUT", "/k", Some(60_000)));
        ok(200, 60_000)(w.request("GET", "/k", None));
    }
    assert_eq!(w.client.connects, 1);
}

/// A server that sends bytes past the body it declared, inside the same
/// record: the extra plaintext must fail the request and must never be read
/// as the response to the next one.
#[test]
fn bytes_past_the_declared_body_are_rejected_over_tls() {
    // Larger than the head buffer, so the body's tail and the extra bytes
    // are still waiting, decrypted, after the head has been read.
    let body_len = HEAD_MAX as usize + 500;
    let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {body_len}\r\n\r\n").into_bytes();
    reply.extend(std::iter::repeat_n(b'b', body_len));
    let honest: &'static [u8] = Box::leak(reply.clone().into_boxed_slice());
    reply.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    let smuggling: &'static [u8] = Box::leak(reply.into_boxed_slice());

    // The same reply without the extra bytes is a good response. (A scripted
    // reply closes its connection, so each case gets its own world.)
    let mut w = world(31, Faults::default());
    w.server.script(Behavior::Raw(honest));
    ok(200, body_len as u32)(w.request("GET", "/k", None));

    let mut w = world(31, Faults::default());
    w.server.script(Behavior::Raw(smuggling));
    assert_eq!(
        w.request("GET", "/k", None),
        Err(Error::Head(longshore::http::head::HttpError::BadFraming))
    );
    // The next request gets its own answer on a new connection.
    ok(404, 0)(w.request("GET", "/missing", None));
}

/// A Host value may carry a port; the name the certificate is checked
/// against may not. The handshake must use the host alone.
#[test]
fn the_server_name_is_the_host_without_its_port() {
    let mut w = world(41, Faults::default());
    let host = format!("{NAME}:8443");
    let req = Request {
        method: "GET",
        target: "/missing",
        host: &host,
        host_id: 0,
        addr: ADDR,
        headers: &[],
        body: None,
        response_buf: Some(RX),
    };
    w.client
        .start(&mut w.io, 500, &req)
        .expect("a connection should be free");
    w.run_until_done(1);
    assert_eq!(
        w.done[0].result.map(|r| r.status),
        Ok(404),
        "the handshake must succeed and the request be answered"
    );
}

/// A fatal alert from the server is a TLS failure with its description,
/// not an orderly close that looks like a stale connection.
#[test]
fn a_fatal_alert_is_reported_as_a_tls_error() {
    let mut w = world(42, Faults::default());
    ok(404, 0)(w.request("GET", "/missing", None));
    // 116 is certificate_required, what a server demanding a client certificate sends.
    w.server.script(Behavior::TlsAlert(116));
    assert_eq!(
        w.request("GET", "/missing", None),
        Err(Error::Tls(TlsError::Alert(116)))
    );
    // The connection is gone; the next request handshakes again and works.
    ok(404, 0)(w.request("GET", "/missing", None));
    assert_eq!(w.client.connects, 2);
}

/// The same over TLS, with the extra bytes in a record of their own: the
/// response fills one record exactly and a second record follows it.
#[test]
fn a_record_after_a_complete_response_is_not_taken_for_the_next_response() {
    let head = |n: usize| format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\n\r\n");
    // Head and body together are one full record of 16384 bytes.
    let body_len = 16384 - head(16000).len();
    let mut reply = head(body_len).into_bytes();
    reply.extend(std::iter::repeat_n(b'b', body_len));
    assert_eq!(reply.len(), 16384);
    reply.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nwrong");
    let reply: &'static [u8] = Box::leak(reply.into_boxed_slice());

    let mut w = world(43, Faults::default());
    w.server.script(Behavior::Raw(reply));
    ok(200, body_len as u32)(w.request("GET", "/k", None));
    ok(404, 0)(w.request("GET", "/missing", None));
    assert_eq!((w.client.idle_dirty, w.client.connects), (1, 2));
}

/// Session tickets arrive after the handshake as records that are not
/// application data. They must not make a healthy idle connection look used.
#[test]
fn an_idle_tls_connection_with_only_tickets_waiting_is_reused() {
    let mut w = world(44, Faults::default());
    for _ in 0..4 {
        ok(404, 0)(w.request("GET", "/missing", None));
    }
    assert_eq!(
        (w.client.connects, w.client.idle_dirty, w.client.idle_closed),
        (1, 0, 0)
    );
}
