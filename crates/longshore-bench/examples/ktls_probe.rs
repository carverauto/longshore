// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Feasibility probe: does handing TLS to the kernel, and through it to the
//! network card, make HTTPS uploads cheaper for the client?
//!
//! Does a TLS 1.3 handshake with rustls, gives the kernel the sending keys
//! (`TLS_TX`), and then sends HTTP PUTs as plaintext on one connection for a
//! few seconds, reporting bytes per second and per second of thread CPU.
//! Responses are not read; the run is short enough that they fit in the
//! socket's receive buffer.
//!
//!   ktls_probe ADDR:PORT CERT.der MODE [SECS] [CPU]
//!
//! MODE is `write` (plain `send`, the kernel copies the bytes) or `sendfile`
//! (from a memory file with `TLS_TX_ZEROCOPY_RO`, no copy when the card
//! encrypts). Blocking sockets and one connection: this probes the kernel
//! path's cost, it is not the client.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ktls_probe needs Linux");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use rustls::pki_types::CertificateDer;
    use rustls::ConnectionTrafficSecrets;
    use std::ffi::{c_int, c_long, c_void};
    use std::io::Write;
    use std::net::TcpStream;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::time::Instant;

    const SOL_TCP: c_int = 6;
    const TCP_ULP: c_int = 31;
    const SOL_TLS: c_int = 282;
    const TLS_TX: c_int = 1;
    const TLS_TX_ZEROCOPY_RO: c_int = 3;
    const TLS_1_3_VERSION: u16 = 0x0304;
    const TLS_CIPHER_AES_GCM_128: u16 = 51;
    const MSG_MORE: c_int = 0x8000;
    const CLOCK_THREAD_CPUTIME_ID: c_int = 3;
    const SIZE: usize = 8 << 20;

    /// `struct tls12_crypto_info_aes_gcm_128` from `linux/tls.h`.
    #[repr(C)]
    struct CryptoInfo {
        version: u16,
        cipher_type: u16,
        iv: [u8; 8],
        key: [u8; 16],
        salt: [u8; 4],
        rec_seq: [u8; 8],
    }

    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }

    extern "C" {
        fn setsockopt(
            fd: c_int,
            level: c_int,
            name: c_int,
            value: *const c_void,
            len: u32,
        ) -> c_int;
        fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
        fn sendfile(out: c_int, input: c_int, offset: *mut c_long, count: usize) -> isize;
        fn memfd_create(name: *const u8, flags: u32) -> c_int;
        fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
        fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int;
        fn sched_setaffinity(pid: c_int, len: usize, mask: *const u64) -> c_int;
    }

    fn die(msg: &str) -> ! {
        eprintln!("ktls_probe: {msg}");
        std::process::exit(1)
    }

    fn thread_cpu_secs() -> f64 {
        let mut ts = Timespec { sec: 0, nsec: 0 };
        unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        ts.sec as f64 + ts.nsec as f64 / 1e9
    }

    /// The kernel's count of connections whose sending side the card encrypts.
    fn device_connections() -> u64 {
        std::fs::read_to_string("/proc/net/tls_stat")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("TlsTxDevice"))
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0)
    }

    fn send_all(fd: c_int, mut bytes: &[u8], flags: c_int) {
        while !bytes.is_empty() {
            let n = unsafe { send(fd, bytes.as_ptr().cast(), bytes.len(), flags) };
            if n <= 0 {
                die(&format!("send: {}", std::io::Error::last_os_error()));
            }
            bytes = &bytes[n as usize..];
        }
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().collect();
        if args.len() < 4 {
            die("usage: ktls_probe ADDR:PORT CERT.der write|sendfile [SECS] [CPU]");
        }
        let (addr, cert, mode) = (&args[1], &args[2], args[3].as_str());
        let secs: f64 = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(5.0);
        if let Some(cpu) = args.get(5).and_then(|v| v.parse::<usize>().ok()) {
            let mut mask = [0u64; 16];
            mask[cpu / 64] |= 1 << (cpu % 64);
            unsafe { sched_setaffinity(0, 128, mask.as_ptr()) };
        }

        // Handshake in user space, AES-128-GCM only so the key layout is fixed.
        let der = std::fs::read(cert).unwrap_or_else(|e| die(&format!("{cert}: {e}")));
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(der))
            .unwrap_or_else(|e| die(&format!("certificate: {e}")));
        // KTLS_VERSION=1.2 speaks TLS 1.2 instead, to see whether the card
        // takes an offload it refuses for 1.3.
        let tls12 = std::env::var("KTLS_VERSION").is_ok_and(|v| v == "1.2");
        use rustls::crypto::aws_lc_rs::cipher_suite as suite;
        let provider = rustls::crypto::CryptoProvider {
            cipher_suites: if tls12 {
                vec![
                    suite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
                    suite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
                ]
            } else {
                vec![suite::TLS13_AES_128_GCM_SHA256]
            },
            ..rustls::crypto::aws_lc_rs::default_provider()
        };
        let versions: &[&rustls::SupportedProtocolVersion] = if tls12 {
            &[&rustls::version::TLS12]
        } else {
            &[&rustls::version::TLS13]
        };
        let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(versions)
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.enable_secret_extraction = true;
        let mut conn =
            rustls::ClientConnection::new(Arc::new(cfg), "bench.test".try_into().unwrap()).unwrap();
        let mut sock = TcpStream::connect(addr).unwrap_or_else(|e| die(&format!("{addr}: {e}")));
        sock.set_nodelay(true).ok();
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)
                .unwrap_or_else(|e| die(&format!("handshake: {e}")));
        }
        while conn.wants_write() {
            conn.write_tls(&mut sock)
                .unwrap_or_else(|e| die(&format!("handshake: {e}")));
        }
        sock.flush().ok();
        let secrets = conn
            .dangerous_extract_secrets()
            .unwrap_or_else(|e| die(&format!("secrets: {e}")));
        let (seq, ConnectionTrafficSecrets::Aes128Gcm { key, iv }) = secrets.tx else {
            die("not AES-128-GCM")
        };

        // Hand the sending keys to the kernel. If the route's card offers TLS
        // offload the kernel uses it by itself; otherwise it encrypts in software.
        let fd = sock.as_raw_fd();
        let before = device_connections();
        let ulp = b"tls";
        if unsafe { setsockopt(fd, SOL_TCP, TCP_ULP, ulp.as_ptr().cast(), ulp.len() as u32) } != 0 {
            die(&format!("TCP_ULP: {}", std::io::Error::last_os_error()));
        }
        let (key, iv) = (key.as_ref(), iv.as_ref());
        let mut info = CryptoInfo {
            version: if tls12 { 0x0303 } else { TLS_1_3_VERSION },
            cipher_type: TLS_CIPHER_AES_GCM_128,
            iv: [0; 8],
            key: [0; 16],
            salt: [0; 4],
            rec_seq: seq.to_be_bytes(),
        };
        info.key.copy_from_slice(key);
        info.salt.copy_from_slice(&iv[..4]);
        info.iv.copy_from_slice(&iv[4..]);
        if unsafe {
            setsockopt(
                fd,
                SOL_TLS,
                TLS_TX,
                (&info as *const CryptoInfo).cast(),
                core::mem::size_of::<CryptoInfo>() as u32,
            )
        } != 0
        {
            die(&format!("TLS_TX: {}", std::io::Error::last_os_error()));
        }
        let on_device = device_connections() > before;

        let body = vec![0x5au8; SIZE];
        let head = format!(
            "PUT /bench/object HTTP/1.1\r\nHost: bench.test\r\nContent-Length: {SIZE}\r\n\r\n"
        );
        let file = if mode == "sendfile" {
            let one: c_int = 1;
            if unsafe {
                setsockopt(
                    fd,
                    SOL_TLS,
                    TLS_TX_ZEROCOPY_RO,
                    (&one as *const c_int).cast(),
                    4,
                )
            } != 0
            {
                die(&format!(
                    "TLS_TX_ZEROCOPY_RO: {} (needs the card to encrypt)",
                    std::io::Error::last_os_error()
                ));
            }
            let m = unsafe { memfd_create(c"longshore-ktls".as_ptr().cast(), 0) };
            if m < 0 || unsafe { write(m, body.as_ptr().cast(), SIZE) } != SIZE as isize {
                die("memfd");
            }
            Some(m)
        } else {
            None
        };

        let (t0, cpu0) = (Instant::now(), thread_cpu_secs());
        let mut requests = 0u64;
        while t0.elapsed().as_secs_f64() < secs {
            send_all(fd, head.as_bytes(), MSG_MORE);
            match file {
                None => send_all(fd, &body, 0),
                Some(m) => {
                    let mut off: c_long = 0;
                    while (off as usize) < SIZE {
                        let n = unsafe { sendfile(fd, m, &mut off, SIZE - off as usize) };
                        if n <= 0 {
                            die(&format!("sendfile: {}", std::io::Error::last_os_error()));
                        }
                    }
                }
            }
            requests += 1;
        }
        let (wall, cpu) = (t0.elapsed().as_secs_f64(), thread_cpu_secs() - cpu0);
        let gb = (requests * SIZE as u64) as f64 / 1e9;
        println!(
            "{{\"cmd\":\"ktls-{mode}\",\"tls\":\"{}\",\"card_encrypts\":{on_device},\"conns\":1,\"secs\":{wall:.2},\"requests\":{requests},\"gb_per_s\":{:.2},\"thread_cpu\":{:.2},\"gb_per_cpu_s\":{:.2}}}",
            if tls12 { "1.2" } else { "1.3" },
            gb / wall,
            cpu / wall,
            gb / cpu.max(1e-9)
        );
    }
}
