// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Generic `Io` driving, buffer pool, temporary path, and loopback helpers.

#[cfg(unix)]
use longshore::io::{engine::Engine, posix::Posix, Limits};
use longshore::io::{BufId, Completion, Completions, Io, Op, Token};
#[cfg(feature = "tls-core")]
use longshore::tls::handshake::{ClientSetup, ServerSetup};
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Pops all currently queued completions from `out` into `dst`.
pub fn drain_completions(out: &mut Completions, dst: &mut Vec<Completion>) -> usize {
    let mut count = 0;
    while let Some(c) = out.pop() {
        dst.push(c);
        count += 1;
    }
    count
}

fn drive_until_inner<IO: Io + ?Sized>(
    io: &mut IO,
    out: &mut Completions,
    max_ticks: usize,
    wait: bool,
    mut pred: impl FnMut(&[Completion]) -> bool,
) -> io::Result<Vec<Completion>> {
    let mut collected = Vec::new();
    let _: usize = drain_completions(out, &mut collected);
    if pred(&collected) {
        return Ok(collected);
    }
    for _ in 0..max_ticks {
        io.tick(wait, out)?;
        let _: usize = drain_completions(out, &mut collected);
        if pred(&collected) {
            return Ok(collected);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("drive_until exceeded {max_ticks} ticks"),
    ))
}

/// Drives `io.tick(true, out)` up to `max_ticks` until `pred` returns `true`.
pub fn drive_until<IO: Io + ?Sized>(
    io: &mut IO,
    out: &mut Completions,
    max_ticks: usize,
    pred: impl FnMut(&[Completion]) -> bool,
) -> io::Result<Vec<Completion>> {
    drive_until_inner(io, out, max_ticks, true, pred)
}

/// Drives `io.tick(false, out)` up to `max_ticks` until `pred` returns `true`.
/// Useful for multi-core single-thread simulation loops.
pub fn drive_until_nonblocking<IO: Io + ?Sized>(
    io: &mut IO,
    out: &mut Completions,
    max_ticks: usize,
    pred: impl FnMut(&[Completion]) -> bool,
) -> io::Result<Vec<Completion>> {
    drive_until_inner(io, out, max_ticks, false, pred)
}

/// Submits `op` under `token` and drives `io` until `token` completes.
/// Returns the matching completion and any other completions observed along the way.
pub fn submit_and_collect_one<IO: Io + ?Sized>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    op: Op,
    max_ticks: usize,
) -> io::Result<(Completion, Vec<Completion>)> {
    io.submit(token, op)
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "Io::submit returned Busy"))?;
    let all = drive_until(io, out, max_ticks, |cs| {
        cs.iter().any(|c| c.token() == token)
    })?;
    let pos = all
        .iter()
        .position(|c| c.token() == token)
        .expect("predicate guaranteed token presence");
    let mut rest = all;
    let matched = rest.remove(pos);
    Ok((matched, rest))
}

/// Submits `op` under `token` and drives `io` until a completion for `token` arrives.
pub fn submit_and_wait_one<IO: Io + ?Sized>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    op: Op,
    max_ticks: usize,
) -> io::Result<Completion> {
    let (matched, _rest) = submit_and_collect_one(io, out, token, op, max_ticks)?;
    Ok(matched)
}

/// Drives `io.tick(true, out)` up to `max_ticks` until `pred` returns `true`,
/// collecting up to `N` completions on the stack without heap allocation.
pub fn drive_until_stack<IO: Io + ?Sized, const N: usize>(
    io: &mut IO,
    out: &mut Completions,
    max_ticks: usize,
    mut pred: impl FnMut(&[Completion]) -> bool,
) -> io::Result<usize> {
    let mut buf = [Completion::new(Token::new(0), 0); N];
    let mut len = 0usize;
    while let Some(c) = out.pop() {
        if len >= N {
            return Err(io::Error::other("drive_until_stack capacity exceeded"));
        }
        buf[len] = c;
        len += 1;
    }
    if pred(&buf[..len]) {
        return Ok(len);
    }
    for _ in 0..max_ticks {
        io.tick(true, out)?;
        while let Some(c) = out.pop() {
            if len >= N {
                return Err(io::Error::other("drive_until_stack capacity exceeded"));
            }
            buf[len] = c;
            len += 1;
        }
        if pred(&buf[..len]) {
            return Ok(len);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "drive_until_stack exceeded max_ticks",
    ))
}

/// Submits `op` under `token` and drives `io` until a completion for `token`
/// arrives, without heap allocation.
pub fn submit_and_wait_one_stack<IO: Io + ?Sized>(
    io: &mut IO,
    out: &mut Completions,
    token: Token,
    op: Op,
    max_ticks: usize,
) -> io::Result<Completion> {
    io.submit(token, op)
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "Io::submit returned Busy"))?;
    while let Some(c) = out.pop() {
        if c.token() == token {
            return Ok(c);
        }
    }
    for _ in 0..max_ticks {
        io.tick(true, out)?;
        while let Some(c) = out.pop() {
            if c.token() == token {
                return Ok(c);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "submit_and_wait_one_stack exceeded max_ticks",
    ))
}

/// Copies `bytes` into pool buffer `buf` starting at byte offset `off`.
pub fn write_pool_bytes<IO: Io + ?Sized>(io: &mut IO, buf: BufId, off: usize, bytes: &[u8]) {
    let dst = io.buf(buf);
    dst[off..off + bytes.len()].copy_from_slice(bytes);
}

/// Writes `path` followed by a trailing NUL byte into pool buffer `buf` at `off`.
/// Returns the total byte length including the trailing NUL (`path.len() + 1`).
pub fn write_nul_path<IO: Io + ?Sized>(io: &mut IO, buf: BufId, off: usize, path: &str) -> u32 {
    let raw = path.as_bytes();
    let total = raw.len() + 1;
    let dst = io.buf(buf);
    assert!(
        off + total <= dst.len(),
        "path ({total} bytes) exceeds buffer slice at offset {off}"
    );
    dst[off..off + raw.len()].copy_from_slice(raw);
    dst[off + raw.len()] = 0;
    total as u32
}

/// Copies `len` bytes from pool buffer `buf` starting at `off` into a new `Vec<u8>`.
pub fn read_pool_bytes<IO: Io + ?Sized>(
    io: &mut IO,
    buf: BufId,
    off: usize,
    len: usize,
) -> Vec<u8> {
    io.buf(buf)[off..off + len].to_vec()
}

/// Copies `dst.len()` bytes from pool buffer `buf` starting at `off` into `dst`
/// without heap allocation.
pub fn read_pool_into<IO: Io + ?Sized>(io: &mut IO, buf: BufId, off: usize, dst: &mut [u8]) {
    let src = io.buf(buf);
    dst.copy_from_slice(&src[off..off + dst.len()]);
}

/// RAII temporary directory guard that removes its directory tree on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a unique directory under `std::env::temp_dir()`.
    pub fn new(prefix: &str) -> io::Result<Self> {
        Self::new_in(&std::env::temp_dir(), prefix)
    }

    /// Creates a unique directory under `LONGSHORE_TEST_DIR` (falling back to `std::env::temp_dir()`).
    #[cfg(target_os = "linux")]
    pub fn new_in_test_dir(prefix: &str) -> io::Result<Self> {
        let base = std::env::var_os("LONGSHORE_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self::new_in(&base, prefix)
    }

    /// Creates a unique directory under `base`.
    pub fn new_in(base: &Path, prefix: &str) -> io::Result<Self> {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("{prefix}_{}_{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { path: dir })
    }

    /// Returns the directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Joins `name` inside this temporary directory.
    pub fn file_path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Joins a short socket filename inside this temporary directory.
    /// Panics if the resulting path exceeds 100 bytes (macOS `sun_path` limit safety).
    pub fn socket_path(&self, name: &str) -> PathBuf {
        let p = self.path.join(name);
        assert!(
            p.to_string_lossy().len() <= 100,
            "Unix socket path too long for sun_path: {}",
            p.display()
        );
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop must not panic over a leftover temporary file.
        let _: Option<()> = std::fs::remove_dir_all(&self.path).ok();
    }
}

fn unused_tcp_addr(bind: &str) -> io::Result<SocketAddr> {
    use std::net::TcpStream;
    use std::time::Duration;
    for _ in 0..64 {
        let addr = {
            let listener = TcpListener::bind(bind)?;
            listener.local_addr()?
        };
        match TcpStream::connect_timeout(&addr, Duration::from_millis(50)) {
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => return Ok(addr),
            Ok(_) | Err(_) => continue,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        "could not obtain a cleanly closed loopback port",
    ))
}

/// Reserves and returns an unused IPv4 loopback socket address, verifying no
/// child process inherited the listening descriptor before returning.
pub fn unused_loopback_addr() -> io::Result<SocketAddr> {
    unused_tcp_addr("127.0.0.1:0")
}

/// Returns the workspace root, honoring `LONGSHORE_REPO_ROOT` when running a
/// cross-compiled test binary on a remote host.
pub fn workspace_root() -> PathBuf {
    if let Some(root) = std::env::var_os("LONGSHORE_REPO_ROOT") {
        return PathBuf::from(root);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Returns the `file_uring` binary path, honoring `LONGSHORE_FILE_URING_BIN`
/// or building it with `cargo` when running locally.
#[cfg(target_os = "linux")]
pub fn file_uring_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("LONGSHORE_FILE_URING_BIN") {
        let path = PathBuf::from(bin);
        assert!(
            path.is_file(),
            "LONGSHORE_FILE_URING_BIN does not point to a file: {}",
            path.display()
        );
        return path;
    }
    let root = workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = std::process::Command::new(&cargo)
        .current_dir(&root)
        .args([
            "build",
            "--quiet",
            "-p",
            "longshore-bench",
            "--bin",
            "file_uring",
        ])
        .output()
        .expect("build file_uring");
    assert!(
        build.status.success(),
        "building file_uring failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("target"));
    target_dir.join("debug/file_uring")
}

/// Constructs a portable `Engine<Posix>` backend configured with `limits`.
#[cfg(unix)]
pub fn posix_backend(limits: Limits) -> io::Result<Engine<Posix>> {
    Ok(Engine::with_world(limits, Posix::new(limits)?))
}

/// Self-signed DER certificate and PKCS#8 key pair for TLS 1.3 tests.
#[cfg(feature = "tls-core")]
#[derive(Clone, Debug)]
pub struct TestIdentity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

#[cfg(feature = "tls-core")]
impl TestIdentity {
    pub fn server_setup(&self) -> ServerSetup {
        ServerSetup::new(&[&self.cert_der], &self.key_der).expect("server setup")
    }
    pub fn client_setup(&self) -> ClientSetup {
        ClientSetup::with_roots(&[&self.cert_der]).expect("client setup")
    }
}

/// Generates an ephemeral self-signed TLS 1.3 test identity for `names`.
#[cfg(feature = "tls-core")]
pub fn make_tls_identity(names: &[&str]) -> TestIdentity {
    let owned: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
    let k = rcgen::generate_simple_self_signed(owned).expect("rcgen cert");
    TestIdentity {
        cert_der: k.cert.der().to_vec(),
        key_der: k.signing_key.serialize_der(),
    }
}
