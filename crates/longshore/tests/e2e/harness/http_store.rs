// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Opaque-box loopback S3-compatible HTTP/1.1 (+ optional TLS 1.3) test server
//! over `std::net::TcpListener` using `longshore::http::head`,
//! `longshore::checksum`, and (`#[cfg(feature = "tls")]`) `longshore::tls`.

#[path = "http_store_s3.rs"]
mod s3;

#[cfg(feature = "tls")]
pub use super::io_driver::{make_tls_identity, TestIdentity};
use longshore::http::head::parse_request;
#[cfg(feature = "tls")]
use longshore::tls::handshake::{Handshake, ServerSetup};
#[cfg(feature = "tls")]
use longshore::tls::record::{
    record_len, Direction, CONTENT_ALERT, CONTENT_APPLICATION_DATA, FRAGMENT_MAX, RECORD_MAX,
};
use s3::dispatch_s3;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(feature = "tls")]
type MaybeTls = Option<Arc<ServerSetup>>;
#[cfg(not(feature = "tls"))]
type MaybeTls = ();

#[cfg(feature = "tls")]
fn clone_tls(t: &MaybeTls) -> MaybeTls {
    t.as_ref().map(Arc::clone)
}
#[cfg(not(feature = "tls"))]
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "MaybeTls is () without the tls feature and an Option<Arc<..>> with it; one signature serves both"
)]
fn clone_tls(_: &MaybeTls) -> MaybeTls {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreObject {
    pub data: Vec<u8>,
    pub etag: String,
    pub crc64_b64: [u8; 12],
}

#[derive(Default)]
pub(super) struct StoreState {
    objects: BTreeMap<String, StoreObject>,
    uploads: BTreeMap<String, (String, BTreeMap<u32, Vec<u8>>)>,
    next_id: u64,
    requests: usize,
}

pub struct HttpStoreServer {
    addr: SocketAddr,
    state: Arc<Mutex<StoreState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HttpStoreServer {
    pub fn start_plain() -> io::Result<Self> {
        #[cfg(feature = "tls")]
        return Self::start_inner(None);
        #[cfg(not(feature = "tls"))]
        return Self::start_inner(());
    }
    #[cfg(feature = "tls")]
    pub fn start_tls(setup: ServerSetup) -> io::Result<Self> {
        Self::start_inner(Some(Arc::new(setup)))
    }
    fn start_inner(tls: MaybeTls) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(StoreState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp) = (Arc::clone(&state), Arc::clone(&stop));
        let thread = thread::spawn(move || accept_loop(&listener, &st, &sp, &tls));
        Ok(Self {
            addr,
            state,
            stop,
            thread: Some(thread),
        })
    }
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    pub fn get_object(&self, key: &str) -> Option<StoreObject> {
        let st = self.state.lock().expect("lock");
        st.objects.get(key.trim_start_matches('/')).cloned()
    }
    pub fn request_count(&self) -> usize {
        self.state.lock().expect("lock").requests
    }
}

impl Drop for HttpStoreServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Only wakes the accept loop, which may already have stopped.
        TcpStream::connect(self.addr).ok();
        if let Some(h) = self.thread.take() {
            // A panicked server thread has already reported it; Drop must not panic again.
            h.join().ok();
        }
    }
}

#[cfg_attr(
    not(feature = "tls"),
    expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "MaybeTls is () without the tls feature and an Option<Arc<..>> with it; one signature serves both"
    )
)]
fn accept_loop(l: &TcpListener, st: &Arc<Mutex<StoreState>>, sp: &Arc<AtomicBool>, tls: &MaybeTls) {
    let mut workers = Vec::new();
    while !sp.load(Ordering::Acquire) {
        match l.accept() {
            Ok((s, _)) => {
                let (st, sp, t) = (Arc::clone(st), Arc::clone(sp), clone_tls(tls));
                workers.push(thread::spawn(move || handle_conn(s, &st, &sp, t)));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(_) => break,
        }
    }
    for w in workers {
        // A panicked connection thread has already reported it; its client saw the failure.
        w.join().ok();
    }
}

fn read_more(s: &mut TcpStream, dst: &mut Vec<u8>, tmp: &mut [u8]) -> io::Result<bool> {
    match s.read(tmp) {
        Ok(0) => Ok(false),
        Ok(n) => Ok({
            dst.extend_from_slice(&tmp[..n]);
            true
        }),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

fn handle_conn(mut s: TcpStream, st: &Mutex<StoreState>, sp: &AtomicBool, tls: MaybeTls) {
    // The peer may already be gone (macOS then refuses socket options with EINVAL);
    // the read loop below ends on a dead socket either way.
    s.set_nonblocking(false).ok();
    s.set_read_timeout(Some(Duration::from_millis(50))).ok();
    #[cfg(feature = "tls")]
    if let Some(setup) = tls {
        // A failed TLS connection just ends; its client sees the failure.
        handle_tls(s, st, sp, &setup).ok();
        return;
    }
    #[cfg(not(feature = "tls"))]
    let () = tls;
    let (mut buf, mut tmp) = (Vec::new(), [0u8; 16 * 1024]);
    while !sp.load(Ordering::Acquire) {
        if let Some((resp, close)) = try_http(&mut buf, st) {
            if s.write_all(&resp).is_err() || close {
                break;
            }
        } else if !matches!(read_more(&mut s, &mut buf, &mut tmp), Ok(true)) {
            break;
        }
    }
}

#[cfg(feature = "tls")]
fn handle_tls(
    mut s: TcpStream,
    st: &Mutex<StoreState>,
    sp: &AtomicBool,
    setup: &ServerSetup,
) -> io::Result<()> {
    let Ok(mut hs) = Handshake::server(setup) else {
        return Ok(());
    };
    let (mut wire, mut plain, mut tmp) = (Vec::new(), Vec::new(), [0u8; RECORD_MAX]);
    while !hs.is_done() && !sp.load(Ordering::Acquire) {
        let n = hs.write(&mut tmp);
        if n > 0 {
            s.write_all(&tmp[..n])?;
            continue;
        }
        let Ok(u) = hs.read(&wire) else {
            return Ok(());
        };
        if u > 0 {
            wire.drain(..u);
        } else if !read_more(&mut s, &mut wire, &mut tmp)? {
            return Ok(());
        }
    }
    let (mut tx, mut rx) = (Direction::new(), Direction::new());
    if hs.finish(&mut tx, &mut rx).is_err() {
        return Ok(());
    }
    while !sp.load(Ordering::Acquire) {
        while let Some(rlen) = record_len(&wire)
            .ok()
            .flatten()
            .filter(|&r| wire.len() >= r)
        {
            let Ok((ct, plen)) = rx.open(&wire[..rlen], &mut tmp) else {
                return Ok(());
            };
            wire.drain(..rlen);
            if ct == CONTENT_ALERT {
                return Ok(());
            }
            if ct == CONTENT_APPLICATION_DATA {
                plain.extend_from_slice(&tmp[..plen]);
            }
        }
        if let Some((out, close)) = try_http(&mut plain, st) {
            for chunk in out.chunks(FRAGMENT_MAX) {
                let Ok(slen) = tx.seal(CONTENT_APPLICATION_DATA, chunk, &mut tmp) else {
                    return Ok(());
                };
                s.write_all(&tmp[..slen])?;
            }
            if close {
                break;
            }
        } else if !matches!(read_more(&mut s, &mut wire, &mut tmp), Ok(true)) {
            break;
        }
    }
    Ok(())
}

fn try_http(buf: &mut Vec<u8>, state: &Mutex<StoreState>) -> Option<(Vec<u8>, bool)> {
    let head = parse_request(buf, 16 * 1024).ok()??;
    let (hl, bl) = (
        head.fields.head_len,
        head.fields.content_length.unwrap_or(0) as usize,
    );
    if buf.len() < hl + bl {
        return None;
    }
    let m = String::from_utf8_lossy(head.method(buf)).into_owned();
    let t = String::from_utf8_lossy(head.target(buf)).into_owned();
    let inm = head.fields.header(buf, "if-none-match") == Some(b"*");
    let rng = head
        .fields
        .header(buf, "range")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|s| s.strip_prefix("bytes=")?.split_once('-'))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));
    let (close, body) = (head.fields.close, buf[hl..hl + bl].to_vec());
    buf.drain(..hl + bl);
    let mut st = state.lock().expect("lock");
    st.requests += 1;
    Some((dispatch_s3(&mut st, &m, &t, inm, rng, body), close))
}
