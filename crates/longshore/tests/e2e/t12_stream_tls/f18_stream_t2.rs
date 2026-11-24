// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feature 18 Tier 2 E2E tests: TLS errors, spill path, close_notify, faults & limits.

#[cfg(feature = "sim")]
use super::super::harness::write_pool_bytes;
#[cfg(all(feature = "sim", feature = "tls-core"))]
use super::super::harness::{drive_until, make_tls_identity, read_pool_bytes};
#[cfg(all(feature = "sim", feature = "tls-core"))]
use super::stream_rig::drive_tls_handshakes;
#[cfg(feature = "sim")]
use super::stream_rig::{connect_pair_sim, CLIENT, SERVER};
#[cfg(feature = "sim")]
use longshore::io::Io;
#[cfg(all(feature = "sim", feature = "tls-core"))]
use longshore::io::Op;
#[cfg(feature = "tls-core")]
use longshore::io::Sock;
#[cfg(any(feature = "sim", feature = "tls-core"))]
use longshore::io::{BufId, Token};
#[cfg(feature = "sim")]
use longshore::io::{Completions, Limits};
#[cfg(feature = "sim")]
use longshore::sim::{Faults, SimIo};
#[cfg(feature = "sim")]
use longshore::stream::Step;
#[cfg(any(feature = "sim", feature = "tls-core"))]
use longshore::stream::Stream;
#[cfg(all(feature = "sim", feature = "tls-core"))]
use longshore::stream::TlsError;
#[cfg(all(feature = "sim", feature = "tls-core"))]
use longshore::tls::handshake::{ClientSetup, Handshake};
#[cfg(feature = "tls-core")]
use longshore::tls::record::{FRAGMENT_MAX, OVERHEAD};

#[cfg(all(feature = "sim", feature = "tls-core"))]
#[test]
fn t2_f18_1_tls_stream_wrong_hostname_returns_bad_certificate() {
    let ident = make_tls_identity(&["expected.test"]);
    let cap = 64 * 1024;
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(4)
        .with_buffer_size(cap)
        .with_ops_max(8);
    let mut io = SimIo::new(limits, 1811, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token::new(1), Token::new(2)).with_tls_buffers(
        BufId::new(0),
        BufId::new(1),
        cap,
        cap,
        4096,
    );
    let mut server = Stream::new(SERVER, Token::new(3), Token::new(4)).with_tls_buffers(
        BufId::new(2),
        BufId::new(3),
        cap,
        cap,
        4096,
    );
    let c_hs = Handshake::client(&ident.client_setup(), "wrong.test").unwrap();
    let s_hs = Handshake::server(&ident.server_setup()).unwrap();
    let (cs, _) = drive_tls_handshakes(&mut io, &mut out, &mut client, &mut server, c_hs, s_hs);
    assert_eq!(cs, Step::Tls(TlsError::BadCertificate));

    let empty_roots = ClientSetup::with_roots(&[]).unwrap();
    assert!(Handshake::client(&empty_roots, "expected.test").is_ok());
}

#[cfg(all(feature = "sim", feature = "tls-core"))]
#[test]
fn t2_f18_2_tls_stream_spill_path_across_small_dst_receives() {
    let ident = make_tls_identity(&["spill.test"]);
    let cap = 64 * 1024;
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(6)
        .with_buffer_size(cap)
        .with_ops_max(8);
    let mut io = SimIo::new(limits, 1812, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token::new(1), Token::new(2)).with_tls_buffers(
        BufId::new(0),
        BufId::new(1),
        cap,
        cap,
        4096,
    );
    let mut server = Stream::new(SERVER, Token::new(3), Token::new(4)).with_tls_buffers(
        BufId::new(2),
        BufId::new(3),
        cap,
        cap,
        4096,
    );
    let c_hs = Handshake::client(&ident.client_setup(), "spill.test").unwrap();
    let s_hs = Handshake::server(&ident.server_setup()).unwrap();
    assert_eq!(
        drive_tls_handshakes(&mut io, &mut out, &mut client, &mut server, c_hs, s_hs),
        (Step::Done(0), Step::Done(0))
    );

    let msg: Vec<u8> = (0..4096u32).map(|i| (i % 199) as u8).collect();
    write_pool_bytes(&mut io, BufId::new(4), 0, &msg);
    client.send(&mut io, BufId::new(4), 0, msg.len() as u32, 4096);
    let mut recvd = Vec::new();
    let mut s_step = server.recv(&mut io, BufId::new(5), 0, 512);
    for _ in 0..200 {
        if let Step::Done(n) = s_step {
            assert!(n > 0);
            recvd.extend_from_slice(&read_pool_bytes(
                &mut io,
                BufId::new(5),
                0,
                usize::try_from(n).unwrap(),
            ));
            if recvd.len() >= msg.len() {
                break;
            }
            assert!(server.has_unread_plaintext());
            s_step = server.recv(&mut io, BufId::new(5), 0, 512);
            continue;
        }
        io.tick(true, &mut out).expect("tick");
        while let Some(c) = out.pop() {
            match c.token().get() {
                1 => {
                    let _: Step = client.on_send(&mut io, c.result());
                }
                4 => s_step = server.on_recv(&mut io, c.result()),
                _ => {}
            }
        }
    }
    assert!(!server.has_unread_plaintext());
    assert_eq!(recvd, msg);
}

#[cfg(all(feature = "sim", feature = "tls-core"))]
#[test]
fn t2_f18_3_tls_stream_close_notify_followed_by_eof_returns_done_zero() {
    let ident = make_tls_identity(&["close.test"]);
    let cap = 64 * 1024;
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(6)
        .with_buffer_size(cap)
        .with_ops_max(8);
    let mut io = SimIo::new(limits, 1813, Faults::default());
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token::new(1), Token::new(2)).with_tls_buffers(
        BufId::new(0),
        BufId::new(1),
        cap,
        cap,
        4096,
    );
    let mut server = Stream::new(SERVER, Token::new(3), Token::new(4)).with_tls_buffers(
        BufId::new(2),
        BufId::new(3),
        cap,
        cap,
        4096,
    );
    let c_hs = Handshake::client(&ident.client_setup(), "close.test").unwrap();
    let s_hs = Handshake::server(&ident.server_setup()).unwrap();
    assert_eq!(
        drive_tls_handshakes(&mut io, &mut out, &mut client, &mut server, c_hs, s_hs),
        (Step::Done(0), Step::Done(0))
    );

    client.send_alert(&mut io, BufId::new(4), 0, 1, 0);
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 20, |v| {
        v.iter().any(|c| c.token().get() == 1)
    })
    .unwrap();
    io.submit(Token::new(99), Op::Close { sock: CLIENT })
        .unwrap();
    let _: Vec<longshore::io::Completion> = drive_until(&mut io, &mut out, 20, |v| {
        v.iter().any(|c| c.token().get() == 99)
    })
    .unwrap();

    let mut s_step = server.recv(&mut io, BufId::new(5), 0, 4096);
    for _ in 0..50 {
        if !matches!(s_step, Step::Pending) {
            break;
        }
        io.tick(true, &mut out).expect("tick");
        while let Some(c) = out.pop() {
            if c.token().get() == 4 {
                s_step = server.on_recv(&mut io, c.result());
            }
        }
    }
    assert_eq!(s_step, Step::Done(0));
}

#[cfg(feature = "sim")]
#[test]
fn t2_f18_4_stream_sim_faults_reset_terminates_cleanly() {
    let limits = Limits::BASE
        .with_sockets_max(4)
        .with_buffers(4)
        .with_buffer_size(4096)
        .with_ops_max(8);
    let faults = Faults::NONE
        .with_reset(200_000)
        .with_partial_recv(300_000)
        .with_short_send(300_000);
    let mut io = SimIo::new(limits, 1814, faults);
    let mut out = Completions::new(16);
    connect_pair_sim(&mut io, &mut out);

    let mut client = Stream::new(CLIENT, Token::new(1), Token::new(2));
    let mut server = Stream::new(SERVER, Token::new(3), Token::new(4));
    write_pool_bytes(&mut io, BufId::new(0), 0, &[0x5a; 4096]);
    client.send(&mut io, BufId::new(0), 0, 4096, 4096);
    let _: Step = server.recv(&mut io, BufId::new(1), 0, 4096);

    let mut saw_end = false;
    for _ in 0..300 {
        io.tick(true, &mut out).expect("tick");
        while let Some(c) = out.pop() {
            match c.token().get() {
                1 => match client.on_send(&mut io, c.result()) {
                    Step::Done(n) if n <= 0 => saw_end = true,
                    Step::Done(_) => client.send(&mut io, BufId::new(0), 0, 4096, 4096),
                    Step::Tls(_) => saw_end = true,
                    Step::Pending => {}
                },
                4 => match server.on_recv(&mut io, c.result()) {
                    Step::Done(n) if n <= 0 => saw_end = true,
                    Step::Done(_) => {
                        let _: Step = server.recv(&mut io, BufId::new(1), 0, 4096);
                    }
                    Step::Tls(_) => saw_end = true,
                    Step::Pending => {}
                },
                _ => {}
            }
        }
        if saw_end {
            break;
        }
    }
    assert!(
        saw_end,
        "expected reset/EOF termination under fault injection"
    );
}

#[cfg(feature = "tls-core")]
#[test]
#[should_panic(expected = "TLS buffers must hold two records")]
fn t2_f18_5_tls_buffers_below_two_records_panics() {
    let min = (2 * (FRAGMENT_MAX + OVERHEAD + 256)) as u32;
    let _: Stream = Stream::new(Sock::new(0), Token::new(1), Token::new(2)).with_tls_buffers(
        BufId::new(0),
        BufId::new(1),
        min - 1,
        min,
        4096,
    );
}
