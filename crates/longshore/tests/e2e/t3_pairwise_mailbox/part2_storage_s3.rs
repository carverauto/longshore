// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Tier 3 pairwise scenarios `T3-23`–`T3-25`: storage-core aligned
//! `submit_chain` over mailbox, `power_loss` closing mailbox, and S3 client
//! plain vs `--no-default-features` TLS rejection.

use super::WriteCmd;
use longshore::io::{BufId, Completion, Completions, FileSlot, Io, Limits, Op, OpenFlags, Token};
use longshore::mailbox::{block_on, mailbox, Config, TryRecv, TrySendError};

#[cfg(feature = "sim")]
use longshore::sim::SimIo;

/// The simulated disk both storage-core scenarios mount.
#[cfg(feature = "sim")]
fn small_disk() -> longshore::sim::DiskConfig {
    longshore::sim::DiskConfig::BASE
        .with_max_files(8)
        .with_max_dirs(4)
        .with_max_bytes_per_file(64 * 1024)
        .with_max_name_len(128)
        .with_write_log_entries(16)
        .with_write_log_bytes(32 * 1024)
        .with_read_error_sectors(4)
        .with_sector_size(512)
}

/// Opens `path` read-write-create into `FileSlot(0)` and waits for the open.
#[cfg(feature = "sim")]
fn open_slot0(core: &mut SimIo, out: &mut Completions, limits: Limits, path: &str, direct: bool) {
    use super::super::harness::{drive_until, write_nul_path};
    let plen = write_nul_path(core, limits.small(0), 0, path);
    core.submit(
        Token::new(1),
        Op::FileOpen {
            slot: FileSlot::new(0),
            path_buf: limits.small(0),
            path_off: 0,
            path_len: plen,
            flags: OpenFlags::NONE
                .with_read(true)
                .with_write(true)
                .with_create(true)
                .with_direct(direct),
        },
    )
    .unwrap();
    let _: Vec<Completion> = drive_until(core, out, 100, |v| v.len() == 1).unwrap();
}

/// The mailbox both storage-core scenarios hand write commands through.
#[cfg(feature = "sim")]
fn write_mailbox_config() -> Config {
    Config::new(4, 2, 4, Token::new(10))
}

/// Submits the aligned write-then-datasync chain that serves `cmd`.
#[cfg(feature = "sim")]
fn submit_write_sync_chain(core: &mut SimIo, limits: Limits, cmd: WriteCmd) {
    use super::super::harness::write_pool_bytes;
    write_pool_bytes(core, limits.aligned(0), 0, &[cmd.fill; 4096]);
    core.submit_chain(&[
        (
            Token::new(20),
            Op::FileWrite {
                slot: FileSlot::new(0),
                buf: limits.aligned(0),
                off: 0,
                len: cmd.len,
                file_off: cmd.file_off,
            },
        ),
        (
            Token::new(21),
            Op::FileSync {
                slot: FileSlot::new(0),
                datasync: true,
            },
        ),
    ])
    .unwrap();
}

#[cfg(feature = "sim")]
#[test]
fn t3_23_mailbox_to_storage_core_aligned_submit_chain() {
    use longshore::sim::Faults;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let limits_a = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(1)
        .with_buffer_size(64)
        .with_ops_max(4);
    let limits_b = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_small_buffers(2)
        .with_small_buffer_size(256)
        .with_files_max(2)
        .with_aligned_buffers(2)
        .with_aligned_buffer_size(4096)
        .with_block_size(4096)
        .with_ops_max(8);
    let mut core_a = SimIo::new(limits_a, 323, Faults::default());
    let mut core_b = SimIo::new(limits_b, 324, Faults::default());
    core_b.setup_disk(small_disk());
    let mut out_b = Completions::new(8);
    open_slot0(&mut core_b, &mut out_b, limits_b, "/core_b.dat", true);

    let (tx_b, mut io_rx_b) =
        mailbox::<WriteCmd, i32, _>(&mut core_b, write_mailbox_config()).unwrap();
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
                    submit_write_sync_chain(&mut core_b, limits_b, cmd);
                    pending_slot = Some(cmd.slot);
                }
            } else if c.token().get() == 21 {
                assert!(io_rx_b
                    .answer(pending_slot.take().unwrap(), c.result())
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
    use super::super::harness::write_pool_bytes;
    use longshore::sim::Faults;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let limits = Limits::BASE
        .with_sockets_max(2)
        .with_buffers(2)
        .with_buffer_size(4096)
        .with_small_buffers(2)
        .with_small_buffer_size(256)
        .with_files_max(2)
        .with_ops_max(8);
    let mut core_b = SimIo::new(limits, 3240, Faults::default());
    core_b.setup_disk(small_disk());
    let mut out_b = Completions::new(8);
    open_slot0(&mut core_b, &mut out_b, limits, "/crash_b.dat", false);

    let (tx_b, mut io_rx_b) =
        mailbox::<WriteCmd, i32, _>(&mut core_b, write_mailbox_config()).unwrap();
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
                write_pool_bytes(&mut core_b, BufId::new(0), 0, &[cmd.fill; 1024]);
                core_b
                    .submit(
                        Token::new(20),
                        Op::FileWrite {
                            slot: FileSlot::new(0),
                            buf: BufId::new(0),
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
    let b = Builder::new("127.0.0.1", "us-east-1", creds)
        .port(server.port())
        .address(Some(server.addr()))
        .bucket_in_path(Some("bucket".into()))
        .tls(false)
        .portable(true)
        .waiters(32)
        .threads(1)
        .buffers(2)
        .buffer_size(64 * 1024)
        .connections(2)
        .operations(4);
    let client = b.build().expect("plain client");
    let mut buf = client.buffer_blocking();
    buf.as_mut_slice()[..5].copy_from_slice(b"hello");
    let (res, buf) = client.store("obj", buf, 5).wait();
    res.expect("store");
    let (len, _) = client.fetch("obj", buf).wait();
    assert_eq!(len.expect("fetch"), Some(5));

    #[cfg(not(feature = "tls"))]
    {
        let bt = Builder::new("127.0.0.1", "us-east-1", creds)
            .port(server.port())
            .address(Some(server.addr()))
            .tls(true)
            .waiters(32);
        match bt.build() {
            Err(BuildError::Setting(msg)) => {
                assert!(msg.contains("tls"), "error must name tls feature: {msg}");
            }
            Err(other) => panic!("expected Setting(\"..tls..\"), got Err({other:?})"),
            Ok(_) => panic!("expected Setting(\"..tls..\"), got Ok(Client)"),
        }
    }
    #[cfg(feature = "tls")]
    let _: BuildError = BuildError::Unsupported;
}
