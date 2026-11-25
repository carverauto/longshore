// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Runs the shared `longshore::tcp` checks on real `io_uring` and on the
//! portable backend of the same kernel, and compares what the two carried.
//! Then a log and its replicas on each: frames received, written with
//! direct I/O to a file in `--dir`, synced and sent to three peers, under
//! the allocation guard. Then both again over TLS on the ring, once with
//! each provider.
//!
//!   tcp_uring [--v6] [--dir <directory on a file system with O_DIRECT>]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("tcp_uring needs Linux (io_uring / epoll)");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[path = "../die.rs"]
mod die;

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: longshore::io::alloc_guard::Guard = longshore::io::alloc_guard::Guard;

/// A hundred rounds of the log and its replicas, all but the first sealed.
#[cfg(target_os = "linux")]
fn log_and_replicate<IO: longshore::io::Io>(
    make: &mut impl FnMut(longshore::io::Limits) -> IO,
    addr: std::net::SocketAddr,
    path: &str,
) {
    use longshore::io::alloc_guard::sealed;
    use longshore::tcp::check::storage::Replicated;
    let mut r = Replicated::new(make, longshore::tcp::Addr::Ip(addr), path);
    r.round(0);
    sealed(|| (1..100).for_each(|i| r.round(i)));
    drop(r);
    // Best-effort cleanup: a leftover file does not change the result.
    let _: Option<()> = std::fs::remove_file(path).ok();
}

/// A certificate for one name, and what both sides need to use it with
/// `provider` sealing and opening the records.
#[cfg(target_os = "linux")]
fn certs(provider: longshore::tls::provider::Provider) -> longshore::tcp::check::Certs {
    use die::OrDie;
    use longshore::tls::handshake::{ClientSetup, ServerSetup};
    let name = "replica.longshore.test";
    let k = rcgen::generate_simple_self_signed(vec![name.to_string()]).or_die("certificate");
    let (cert, key) = (k.cert.der().to_vec(), k.signing_key.serialize_der());
    let client = ClientSetup::with_roots_using(provider, &[&cert]).or_die("client setup");
    let server = ServerSetup::new_using(provider, &[&cert], &key).or_die("server setup");
    longshore::tcp::check::Certs::new(client, server, name)
}

/// The log and its replicas over TLS on the ring: only the handshakes,
/// which are over before the seal, allocate.
#[cfg(target_os = "linux")]
fn log_and_replicate_tls(
    addr: std::net::SocketAddr,
    path: &str,
    certs: &longshore::tcp::check::Certs,
) {
    use die::OrDie;
    use longshore::io::alloc_guard::sealed;
    use longshore::io::uring::UringIo;
    use longshore::io::Limits;
    use longshore::tcp::check::storage::Replicated;
    use longshore::tcp::check::{secure, World};
    let w = World::new_tls(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        Limits::BASE.with_files_max(1),
        secure::sizes(4, 0, 0),
        secure::sizes(2, 4, 2),
        certs,
    );
    let mut r = Replicated::over(w, longshore::tcp::Addr::Ip(addr), path);
    r.round(0);
    sealed(|| (1..100).for_each(|i| r.round(i)));
    drop(r);
    // Best-effort cleanup: a leftover file does not change the result.
    let _: Option<()> = std::fs::remove_file(path).ok();
}

/// What `Net` relies on from the transport, checked on the transport
/// itself, on the ring and on the portable backend.
#[cfg(target_os = "linux")]
fn conforms(addr: std::net::SocketAddr) {
    use die::OrDie;
    use longshore::io::posix::PosixIo;
    use longshore::io::uring::UringIo;
    use longshore::tcp::check::conformance::run_all;
    run_all(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
    );
    run_all(
        &mut |limits| PosixIo::new(limits).or_die("PosixIo::new"),
        addr,
    );
}

/// Every check and the log over TLS on the ring, with `provider` sealing
/// and opening the records: the frames `plain` carried.
#[cfg(target_os = "linux")]
fn tls_on_the_ring(
    provider: longshore::tls::provider::Provider,
    addr: std::net::SocketAddr,
    plain: &[Vec<u8>],
    unix_path: &str,
    log_path: &str,
) {
    use die::OrDie;
    use longshore::io::uring::UringIo;
    let certs = certs(provider);
    let over_tls = longshore::tcp::check::run_all_tls(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
        unix_path,
        &certs,
    );
    if over_tls != plain {
        let tag = provider.feature();
        die::die(format_args!("TLS with {tag} carried different frames"));
    }
    log_and_replicate_tls(addr, log_path, &certs);
    // And what `Net` relies on from the transport, handshakes included.
    longshore::tcp::check::conformance::run_all_secured(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
        &certs,
    );
}

#[cfg(target_os = "linux")]
fn main() {
    use die::OrDie;
    use longshore::io::posix::PosixIo;
    use longshore::io::uring::UringIo;
    use longshore::tcp::check;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let mut v6 = false;
    let mut dir = std::env::temp_dir();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--v6" => v6 = true,
            "--dir" => dir = args.next().or_die("--dir needs a directory").into(),
            other => die::die(format_args!("unknown flag {other}")),
        }
    }
    let ip = if v6 {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    let addr = SocketAddr::new(ip, 0);
    let path = |tag: &str| {
        let name = format!("longshore-tcp-{tag}-{}.sock", std::process::id());
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    };

    let ring = check::run_all(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
        &path("ring"),
    );
    let portable = check::run_all(
        &mut |limits| PosixIo::new(limits).or_die("PosixIo::new"),
        addr,
        &path("posix"),
    );
    if ring.is_empty() || ring != portable {
        die::die("the ring and the portable backend carried different frames");
    }
    conforms(addr);
    let log = |tag: &str| {
        let name = format!("longshore-tcp-{tag}-{}.dat", std::process::id());
        dir.join(name).to_string_lossy().into_owned()
    };
    log_and_replicate(
        &mut |limits| UringIo::new(limits).or_die("UringIo::new"),
        addr,
        &log("ring"),
    );
    log_and_replicate(
        &mut |limits| PosixIo::new(limits).or_die("PosixIo::new"),
        addr,
        &log("posix"),
    );
    for provider in longshore::tls::provider::Provider::ALL {
        let tag = provider.feature();
        tls_on_the_ring(provider, addr, &ring, &path(tag), &log(tag));
    }
    println!(
        "ok tcp_uring v6={v6} frames={} logged=200 tls=3",
        ring.len()
    );
}
