// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-23`–`T3-25`: storage-core aligned
//! `submit_chain` over mailbox, `power_loss` closing mailbox, and S3 client
//! plain vs `--no-default-features` TLS rejection.

#[allow(unused_imports)]
use super::WriteCmd;
#[allow(unused_imports)]
use longshore::io::{BufId, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
#[allow(unused_imports)]
use longshore::mailbox::{block_on, mailbox, Config, TryRecv, TrySendError};

#[cfg(feature = "sim")]
#[test]
fn t3_23_mailbox_to_storage_core_aligned_submit_chain() {
    use super::super::harness::{drive_until, write_nul_path, write_pool_bytes};
    use longshore::sim::{Faults, SimIo};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let limits_a = Limits {
        sockets_max: 2,
        buffers: 1,
        buffer_size: 64,
        ops_max: 4,
        ..Limits::BASE
    };
    let limits_b = Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 256,
        files_max: 2,
        aligned_buffers: 2,
        aligned_buffer_size: 4096,
        block_size: 4096,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut core_a = SimIo::new(limits_a, 323, Faults::default());
    let mut core_b = SimIo::new(limits_b, 324, Faults::default());
    core_b.setup_disk(longshore::sim::DiskConfig {
        max_files: 8,
        max_dirs: 4,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: 32 * 1024,
        read_error_sectors: 4,
        sector_size: 512,
    });
    let mut out_b = Completions::new(8);

    let plen = write_nul_path(&mut core_b, limits_b.small(0), 0, "/core_b.dat");
    core_b
        .submit(
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: limits_b.small(0),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    direct: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    let _ = drive_until(&mut core_b, &mut out_b, 100, |v| v.len() == 1).unwrap();

    let (tx_b, mut io_rx_b) = mailbox::<WriteCmd, i32, _>(
        &mut core_b,
        Config {
            capacity: 4,
            slots: 2,
            waiters_prealloc: 4,
            wake_token: Token(10),
        },
    )
    .unwrap();
    io_rx_b.arm(&mut core_b).unwrap();
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let mut fut = pin!(tx_b.call(
        |slot| WriteCmd {
            slot,
            file_off: 0,
            len: 4096,
            fill: 0x6D,
        },
        -999,
    ));
    assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

    let mut pending_slot = None;
    let mut answered = false;
    let mut out_a = Completions::new(4);
    for _ in 0..32 {
        core_a.tick(false, &mut out_a).unwrap();
        core_b.tick(false, &mut out_b).unwrap();
        while let Some(c) = out_b.pop() {
            if io_rx_b.on_completion(&mut core_b, c).unwrap() {
                while let TryRecv::Message(cmd) = io_rx_b.try_recv() {
                    write_pool_bytes(&mut core_b, limits_b.aligned(0), 0, &[cmd.fill; 4096]);
                    core_b
                        .submit_chain(&[
                            (
                                Token(20),
                                Op::FileWrite {
                                    slot: FileSlot(0),
                                    buf: limits_b.aligned(0),
                                    off: 0,
                                    len: cmd.len,
                                    file_off: cmd.file_off,
                                },
                            ),
                            (
                                Token(21),
                                Op::FileSync {
                                    slot: FileSlot(0),
                                    datasync: true,
                                },
                            ),
                        ])
                        .unwrap();
                    pending_slot = Some(cmd.slot);
                }
            } else if c.token == Token(21) {
                assert!(io_rx_b
                    .answer(pending_slot.take().unwrap(), c.result)
                    .is_none());
                answered = true;
                break;
            }
        }
        if answered {
            break;
        }
    }
    let ans = block_on(fut);
    assert_eq!(ans, 0);
}

#[cfg(feature = "sim")]
#[test]
fn t3_24_storage_core_power_loss_closes_mailbox_and_settles_disk() {
    use super::super::harness::{drive_until, write_nul_path, write_pool_bytes};
    use longshore::sim::{Faults, SimIo};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let limits = Limits {
        sockets_max: 2,
        buffers: 2,
        buffer_size: 4096,
        small_buffers: 2,
        small_buffer_size: 256,
        files_max: 2,
        ops_max: 8,
        ..Limits::BASE
    };
    let mut core_b = SimIo::new(limits, 3240, Faults::default());
    core_b.setup_disk(longshore::sim::DiskConfig {
        max_files: 8,
        max_dirs: 4,
        max_bytes_per_file: 64 * 1024,
        max_name_len: 128,
        write_log_entries: 16,
        write_log_bytes: 32 * 1024,
        read_error_sectors: 4,
        sector_size: 512,
    });
    let mut out_b = Completions::new(8);

    let plen = write_nul_path(&mut core_b, limits.small(0), 0, "/crash_b.dat");
    core_b
        .submit(
            Token(1),
            Op::FileOpen {
                slot: FileSlot(0),
                path_buf: limits.small(0),
                path_off: 0,
                path_len: plen,
                flags: OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    let _ = drive_until(&mut core_b, &mut out_b, 100, |v| v.len() == 1).unwrap();

    let (tx_b, mut io_rx_b) = mailbox::<WriteCmd, i32, _>(
        &mut core_b,
        Config {
            capacity: 4,
            slots: 2,
            waiters_prealloc: 4,
            wake_token: Token(10),
        },
    )
    .unwrap();
    io_rx_b.arm(&mut core_b).unwrap();
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let mut fut = pin!(tx_b.call(
        |slot| WriteCmd {
            slot,
            file_off: 0,
            len: 1024,
            fill: 0x5E,
        },
        -999,
    ));
    assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

    core_b.tick(false, &mut out_b).unwrap();
    while let Some(c) = out_b.pop() {
        if io_rx_b.on_completion(&mut core_b, c).unwrap() {
            while let TryRecv::Message(cmd) = io_rx_b.try_recv() {
                write_pool_bytes(&mut core_b, BufId(0), 0, &[cmd.fill; 1024]);
                core_b
                    .submit(
                        Token(20),
                        Op::FileWrite {
                            slot: FileSlot(0),
                            buf: BufId(0),
                            off: 0,
                            len: cmd.len,
                            file_off: cmd.file_off,
                        },
                    )
                    .unwrap();
            }
        }
    }

    core_b.power_loss();
    io_rx_b.close(|_| {}, || -999, |_| {});
    assert_eq!(block_on(fut), -999);
    assert!(matches!(
        tx_b.try_send(WriteCmd {
            slot: 0,
            file_off: 0,
            len: 512,
            fill: 1
        }),
        Err(TrySendError::Closed(_))
    ));
}

#[test]
fn t3_25_s3_client_on_mailbox_plain_vs_tls_feature_gate() {
    use super::super::harness::HttpStoreServer;
    use longshore::client::{BuildError, Builder};
    use longshore::store::creds::Credentials;

    let server = HttpStoreServer::start_plain().expect("plain store");
    let creds = Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    )
    .unwrap();
    let mut b = Builder::new("127.0.0.1", "us-east-1", creds);
    b.port = server.port();
    b.address = Some(server.addr());
    b.bucket_in_path = Some("bucket".into());
    b.tls = false;
    #[cfg(target_os = "linux")]
    {
        b.portable = true;
    }
    b.waiters = 32;
    b.threads = 1;
    b.buffers = 2;
    b.buffer_size = 64 * 1024;
    b.connections = 2;
    b.operations = 4;
    let client = b.build().expect("plain client");
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..5].copy_from_slice(b"hello");
    let (res, buf) = client.store("obj", buf, 5).wait();
    res.expect("store");
    let (len, _) = client.fetch("obj", buf).wait();
    assert_eq!(len.expect("fetch"), Some(5));

    #[cfg(not(feature = "tls"))]
    {
        let mut bt = Builder::new("127.0.0.1", "us-east-1", creds);
        bt.port = server.port();
        bt.address = Some(server.addr());
        bt.tls = true;
        bt.waiters = 32;
        match bt.build() {
            Err(BuildError::Setting(msg)) => {
                assert!(msg.contains("tls"), "error must name tls feature: {msg}");
            }
            Err(other) => panic!("expected Setting(\"..tls..\"), got Err({other:?})"),
            Ok(_) => panic!("expected Setting(\"..tls..\"), got Ok(Client)"),
        }
    }
    #[cfg(feature = "tls")]
    let _ = BuildError::Unsupported;
}
