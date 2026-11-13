// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! HTTP client against the in-tree server on one simulated `Io`, with heap
//! allocation forbidden while requests run.

mod common;
use common::*;

#[test]
fn server_failures_surface_as_errors_and_the_pool_recovers() {
    let cases: [(Behavior, Error); 5] = [
        (
            Behavior::CloseBeforeResponse,
            Error::Closed { reused: true },
        ),
        (Behavior::TruncateBody, Error::Closed { reused: true }),
        (Behavior::GarbageHead, Error::Head(HttpError::BadStartLine)),
        (Behavior::EndlessHead, Error::Head(HttpError::HeadTooLarge)),
        (Behavior::BadChunk, Error::Head(HttpError::BadFraming)),
    ];
    for (behavior, expected) in cases {
        let mut w = world(5, Faults::default());
        w.fill_tx(1000, 0);
        // A first request so the failing one runs on a reused connection.
        ok(200, 0)(w.request("PUT", "/k", Some(1000)));
        w.server.script(behavior);
        assert_eq!(w.request("GET", "/k", None), Err(expected), "{behavior:?}");
        w.settle(50);
        ok(200, 1000)(w.request("GET", "/k", None));
        assert_eq!(
            w.client.in_flight(),
            0,
            "{behavior:?} left a request in flight"
        );
    }
}

#[test]
fn a_stalled_server_times_out_and_the_slot_is_freed() {
    let mut w = world(6, Faults::default());
    w.server.script(Behavior::Stall);
    let t0 = w.io.now_ns();
    assert_eq!(w.request("GET", "/k", None), Err(Error::Timeout));
    let waited = (w.io.now_ns() - t0) / MS;
    assert!(
        (2000..2100).contains(&waited),
        "timed out after {waited} ms, expected about 2000"
    );
    w.settle(50);
    ok(404, 0)(w.request("GET", "/k", None));
    assert_eq!(w.client.in_flight(), 0);
}

#[test]
fn a_timed_out_request_ends_even_if_the_cancelled_read_returns_data() {
    // The peer answers with a partial response exactly when the timeout
    // fires, then goes silent. Depending on the seed, the client's read
    // completes with that data before the cancel reaches it.
    const PEER: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 81), 80);
    const LISTEN: Sock = Sock(1 + CONNS * 2);
    const PEER_SOCK: Sock = Sock(2 + CONNS * 2);
    const ACCEPT: u64 = 3 << 40;
    const RECV: u64 = ACCEPT + 1;
    const SEND: u64 = ACCEPT + 2;
    const PARTIAL: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nhalf";
    const OUT_OFF: u32 = 4096;
    let timeout = client_cfg().request_timeout_ns;
    for seed in 0..64 {
        let mut w = world(seed, Faults::default());
        w.io.listen(LISTEN, PEER).unwrap();
        w.io.submit(
            Token(ACCEPT),
            Op::Accept {
                listener: LISTEN,
                sock: PEER_SOCK,
            },
        )
        .unwrap();
        w.io.buf(TX)[OUT_OFF as usize..OUT_OFF as usize + PARTIAL.len()].copy_from_slice(PARTIAL);
        let req = Request {
            method: "GET",
            target: "/slow",
            host: "slow.test",
            host_id: 1,
            addr: PEER,
            headers: &[],
            body: None,
            response_buf: Some(RX),
        };
        let t0 = w.io.now_ns();
        w.client.start(&mut w.io, 7, &req).unwrap();
        let (mut sent, mut peer_eof, mut result) = (false, false, None);
        let World {
            io,
            client,
            server,
            out,
            ..
        } = &mut w;
        sealed(|| {
            while io.now_ns() - t0 < 5 * timeout && !(peer_eof && result.is_some()) {
                io.tick(true, out).expect("simulation deadlock");
                while let Some(c) = out.pop() {
                    match c.token.0 {
                        HEARTBEAT => {
                            if !sent && io.now_ns() - t0 >= timeout {
                                io.submit(
                                    Token(SEND),
                                    Op::Send {
                                        sock: PEER_SOCK,
                                        buf: TX,
                                        off: OUT_OFF,
                                        len: PARTIAL.len() as u32,
                                        zero_copy: false,
                                    },
                                )
                                .unwrap();
                                sent = true;
                            }
                            client.tick(io);
                            io.submit(Token(HEARTBEAT), Op::Timeout { nanos: 10 * MS })
                                .unwrap();
                        }
                        ACCEPT | RECV if c.result > 0 || c.token.0 == ACCEPT => {
                            io.submit(
                                Token(RECV),
                                Op::Recv {
                                    sock: PEER_SOCK,
                                    buf: TX,
                                    off: 0,
                                    len: 1024,
                                },
                            )
                            .unwrap();
                        }
                        RECV => peer_eof = true,
                        SEND => {}
                        _ => match client.on_completion(io, c) {
                            Handled::Done(d) => result = Some(d),
                            Handled::Pending => {}
                            Handled::NotMine => {
                                assert!(server.on_completion(io, c), "unrouted completion {c:?}")
                            }
                        },
                    }
                }
            }
        });
        assert!(sent, "seed {seed}: the partial response was never sent");
        assert_eq!(
            result,
            Some(Done {
                token: 7,
                result: Err(Error::Timeout)
            }),
            "seed {seed}: the request did not end with a timeout"
        );
        assert!(
            peer_eof,
            "seed {seed}: the client did not close the connection"
        );
        assert_eq!(w.client.in_flight(), 0, "seed {seed}");
    }
}

#[test]
fn connect_refused_is_reported_without_leaking_the_slot() {
    let mut w = world(
        7,
        Faults {
            connect_refused: 1_000_000,
            ..Faults::default()
        },
    );
    for _ in 0..CONNS * 2 {
        assert_eq!(
            w.request("GET", "/k", None),
            Err(Error::Io(osc_io::ECONNREFUSED))
        );
    }
    assert_eq!(w.client.in_flight(), 0);
}

/// Short writes, partial reads and reordering must not change what is stored or fetched.
#[test]
fn round_trip_survives_short_writes_partial_reads_and_delays() {
    let faults = Faults {
        short_send: 300_000,
        partial_recv: 300_000,
        delay: 200_000,
        ..Faults::default()
    };
    for seed in 0..40 {
        let mut w = world(seed, faults);
        w.fill_tx(BUF, seed as u8);
        ok(200, 0)(w.request("PUT", "/k", Some(BUF)));
        ok(200, BUF)(w.request("GET", "/k", None));
        let sent = w.io.buf(TX).to_vec();
        assert!(
            w.io.buf(RX)[..] == sent[..],
            "seed {seed}: fetched bytes differ"
        );
        assert_eq!(
            w.server.object("/k"),
            Some(&sent[..]),
            "seed {seed}: stored bytes differ"
        );
    }
}

/// With connection resets injected, every request still ends, in success or
/// an error, and the pool never leaks a connection slot.
#[test]
fn resets_never_hang_a_request_or_leak_a_slot() {
    let faults = Faults {
        reset: 30_000,
        short_send: 100_000,
        partial_recv: 100_000,
        delay: 100_000,
        ..Faults::default()
    };
    let (mut oks, mut errs) = (0, 0);
    for seed in 0..40 {
        let mut w = world(seed, faults);
        w.fill_tx(BUF, 1);
        for i in 0..12 {
            let r = if i % 2 == 0 {
                w.request("PUT", "/k", Some(BUF))
            } else {
                w.request("GET", "/k", None)
            };
            match r {
                Ok(resp) => {
                    assert!(
                        matches!(resp.status, 200 | 404),
                        "seed {seed}: status {}",
                        resp.status
                    );
                    oks += 1;
                }
                Err(Error::Io(_) | Error::Closed { .. } | Error::Timeout) => errs += 1,
                Err(e) => panic!("seed {seed}: unexpected error {e:?}"),
            }
            w.settle(20);
            assert_eq!(
                w.client.in_flight(),
                0,
                "seed {seed}: a request is still marked in flight"
            );
        }
    }
    assert!(
        oks > 50 && errs > 50,
        "expected a mix of outcomes, got {oks} ok and {errs} errors"
    );
}

/// Model-based property (Hegel): for any seed, fault mix and sequence of
/// operations, the client agrees with a trivial model of one object.
///
/// - A successful GET returns exactly the bytes of the last PUT known to have
///   succeeded, unless a PUT failed in between, after which either version
///   may be stored.
/// - Every request ends, and no connection slot is left in flight.
#[hegel::test]
fn client_agrees_with_a_model_under_faults(tc: hegel::TestCase) {
    use hegel::generators as gs;
    let seed = tc.draw(gs::integers::<u64>());
    let ppm =
        |tc: &hegel::TestCase, max: u32| tc.draw(gs::integers::<u32>().min_value(0).max_value(max));
    let faults = Faults {
        reset: ppm(&tc, 20_000),
        short_send: ppm(&tc, 500_000),
        partial_recv: ppm(&tc, 500_000),
        delay: ppm(&tc, 400_000),
        connect_refused: ppm(&tc, 100_000),
        ..Faults::default()
    };
    let ops = tc.draw(
        gs::vecs(
            gs::integers::<u32>()
                .min_value(0)
                .max_value(3 * (BUF + 1) - 1),
        )
        .min_size(1)
        .max_size(12),
    );

    let mut w = world(seed, faults);
    // What the server may hold: `Some(candidates)` once anything was attempted.
    let mut known: Option<(u32, u8)> = None;
    let mut maybe: Vec<(u32, u8)> = Vec::new();
    let mut exists_for_sure = false;
    for (step, op) in ops.into_iter().enumerate() {
        let (kind, len) = (op / (BUF + 1), op % (BUF + 1));
        let salt = step as u8;
        match kind {
            0 => {
                w.fill_tx(len, salt);
                match w.request("PUT", "/k", Some(len)) {
                    Ok(r) => {
                        assert_eq!(r.status, 200, "seed {seed} step {step}");
                        known = Some((len, salt));
                        maybe.clear();
                        exists_for_sure = true;
                    }
                    // The server may or may not have stored it.
                    Err(Error::Io(_) | Error::Closed { .. } | Error::Timeout) => {
                        maybe.push((len, salt))
                    }
                    Err(e) => panic!("seed {seed} step {step}: PUT failed with {e:?}"),
                }
            }
            1 => match w.request("GET", "/k", None) {
                Ok(r) if r.status == 200 => {
                    let got = w.io.buf(RX)[..r.body_len as usize].to_vec();
                    let matches = |(len, salt): (u32, u8), w: &mut World| {
                        w.fill_tx(len, salt);
                        len == r.body_len && w.io.buf(TX)[..len as usize] == got[..]
                    };
                    let candidates: Vec<(u32, u8)> =
                        known.into_iter().chain(maybe.iter().copied()).collect();
                    assert!(
                        candidates.into_iter().any(|c| matches(c, &mut w)),
                        "seed {seed} step {step}: GET returned {} bytes matching no PUT",
                        r.body_len
                    );
                }
                Ok(r) => {
                    assert_eq!(r.status, 404, "seed {seed} step {step}");
                    assert!(
                        !exists_for_sure,
                        "seed {seed} step {step}: 404 for an object a PUT confirmed"
                    );
                }
                Err(Error::Io(_) | Error::Closed { .. } | Error::Timeout) => {}
                Err(e) => panic!("seed {seed} step {step}: GET failed with {e:?}"),
            },
            _ => {
                // Idle time lets closes complete and exercises reuse after a pause.
                w.settle(len as u64 % 40);
            }
        }
        w.settle(5);
        assert_eq!(
            w.client.in_flight(),
            0,
            "seed {seed} step {step}: request left in flight"
        );
    }
}
