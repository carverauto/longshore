// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Building and sending one attempt of an operation, in the store vendor's request form.

use super::*;

impl S3 {
    /// Sign and send one attempt for slot `i`.
    pub(super) fn attempt<IO: Io>(&mut self, io: &mut IO, i: usize) {
        let addr = match &mut self.cfg.endpoint {
            Endpoint::Fixed(a) => Some(*a),
            Endpoint::Resolved(r) => r.lookup(0).map(|ip| SocketAddrV4::new(ip, self.cfg.port)),
        };
        // No address yet: wait a little rather than fail. This does not count
        // as an attempt, but it is not allowed to go on for ever: a name that
        // never resolves would otherwise hold the slot for good. The limit is
        // the time the operation's attempts could have taken.
        let Some(addr) = addr else {
            let waits_max = (self
                .cfg
                .http
                .request_timeout_ns
                .saturating_mul(self.cfg.attempts_max as u64)
                / self.cfg.backoff_ns.max(1))
            .max(1);
            if self.slots[i].address_waits as u64 >= waits_max {
                self.slots[i].state = State::Finished(Outcome::Fatal(Fatal::NoAddress));
                return;
            }
            self.slots[i].address_waits += 1;
            return self.wait(io, i, self.cfg.backoff_ns);
        };

        let s = self.slots[i];
        let path = core::str::from_utf8(&s.path[..s.path_len]).expect("encoded keys are ASCII");
        let date = amz_date(io.unix_secs());
        let date_str = core::str::from_utf8(&date).expect("timestamps are ASCII");
        let (method, payload) = match s.kind {
            Kind::Store { .. } if s.comparing => ("HEAD", EMPTY_SHA256),
            Kind::Store { .. } => ("PUT", UNSIGNED_PAYLOAD),
            Kind::Fetch { .. } | Kind::List => ("GET", EMPTY_SHA256),
            Kind::Erase => ("DELETE", EMPTY_SHA256),
        };

        // "bytes=a-b" without allocating.
        let mut range_buf = [0u8; 48];
        let mut range_len = 0;
        if let Kind::Fetch {
            range: Some((a, b)),
        } = s.kind
        {
            let mut put = |bytes: &[u8]| {
                range_buf[range_len..range_len + bytes.len()].copy_from_slice(bytes);
                range_len += bytes.len();
            };
            let digits = |mut v: u64, out: &mut [u8; 20]| -> usize {
                let mut k = 20;
                loop {
                    k -= 1;
                    out[k] = b'0' + (v % 10) as u8;
                    v /= 10;
                    if v == 0 {
                        return k;
                    }
                }
            };
            let mut d = [0u8; 20];
            put(b"bytes=");
            let k = digits(a, &mut d);
            put(&d[k..]);
            put(b"-");
            let k = digits(b, &mut d);
            put(&d[k..]);
        }
        let range = core::str::from_utf8(&range_buf[..range_len]).expect("ASCII");

        // Sent with the credentials in use now. A request already sent keeps
        // what it carries; only later attempts see a rotation.
        let creds = &self.creds[if s.candidate {
            1 - self.active
        } else {
            self.active
        }];
        let mut auth = [0u8; 7 + TOKEN_MAX];
        let mut headers: [(&str, &[u8]); 7] = [("", b""); 7];
        let goog_hash;
        // S3: headers that are both sent and signed.
        let crc_value = s.crc.map(crate::checksum::crc64_header_value);
        let crc_str = crc_value
            .as_ref()
            .map(|v| core::str::from_utf8(v).expect("base64 is ASCII"));
        let mut signed: [(&str, &str); 7] = [
            ("host", &self.cfg.host),
            ("x-amz-content-sha256", payload),
            ("x-amz-date", date_str),
            ("", ""),
            ("", ""),
            ("", ""),
            ("", ""),
        ];
        let n = if self.cfg.vendor == Vendor::Gcs {
            // No signature: the token is the whole proof, so nothing depends
            // on the other headers or on the clock.
            let mut k = 0;
            if let (Kind::Store { .. }, Some(crc), false) = (s.kind, s.crc, s.comparing) {
                let mut v = *b"crc32c=AAAAAAAA";
                v[7..].copy_from_slice(&crate::checksum::crc32c_header_value(crc as u32));
                goog_hash = v;
                headers[k] = ("x-goog-hash", &goog_hash);
                k += 1;
            }
            if !range.is_empty() {
                headers[k] = ("range", range.as_bytes());
                k += 1;
            }
            if s.kind == (Kind::Store { no_clobber: true }) && !s.comparing {
                headers[k] = ("x-goog-if-generation-match", b"0");
                k += 1;
            }
            auth[..7].copy_from_slice(b"Bearer ");
            let token = creds.secret_key().as_bytes();
            auth[7..7 + token.len()].copy_from_slice(token);
            headers[k] = ("authorization", &auth[..7 + token.len()]);
            k + 1
        } else {
            let mut m = 3;
            if let (Kind::Store { .. }, Some(v), false) = (s.kind, crc_str, s.comparing) {
                signed[m] = (CHECKSUM_HEADER, v);
                m += 1;
            }
            // Ask the store to return the object's checksum with a whole-object
            // fetch, and when comparing a stored object with ours.
            if s.kind == (Kind::Fetch { range: None }) || s.comparing {
                signed[m] = ("x-amz-checksum-mode", "ENABLED");
                m += 1;
            }
            if !range.is_empty() {
                signed[m] = ("range", range);
                m += 1;
            }
            if s.kind == (Kind::Store { no_clobber: true }) && !s.comparing {
                signed[m] = ("if-none-match", "*");
                m += 1;
            }
            // Temporary keys are only valid together with their session
            // token, which is sent and signed like any other header.
            if !creds.session_token().is_empty() {
                signed[m] = ("x-amz-security-token", creds.session_token());
                m += 1;
            }
            let scope = sigv4::Scope {
                access_key: creds.access_key(),
                secret_key: creds.secret_key(),
                region: &self.cfg.region,
                service: "s3",
                amz_date: &date,
            };
            // A listing's target carries a query; it is signed as sorted pairs.
            let (uri, query) = match path.split_once('?') {
                Some((uri, query)) if s.kind == Kind::List => (uri, query),
                _ => (path, ""),
            };
            let mut pairs = [("", ""); 4];
            let mut pair_count = 0;
            for kv in query.split('&').filter(|kv| !kv.is_empty()) {
                pairs[pair_count] = kv.split_once('=').unwrap_or((kv, ""));
                pair_count += 1;
            }
            let sreq = sigv4::Request {
                method,
                canonical_uri: uri,
                query: &pairs[..pair_count],
                headers: &signed[..m],
                payload_hash: payload,
            };
            let auth_len = authorization(&sreq, &scope, &mut auth)
                .expect("seven headers and the Authorization buffer always fit");

            // Host is added by the HTTP client; send the rest plus Authorization.
            for (k, (name, value)) in signed[1..m].iter().enumerate() {
                headers[k] = (name, value.as_bytes());
            }
            headers[m - 1] = ("authorization", &auth[..auth_len]);
            m
        };
        let req = Request {
            method,
            target: path,
            host: &self.cfg.host,
            host_id: 0,
            addr,
            headers: &headers[..n],
            body: s.body.filter(|_| !s.comparing),
            response_buf: s.response_buf,
        };

        match self.client.start(io, i as u64, &req) {
            Ok(()) => {
                self.slots[i].attempts += 1;
                self.slots[i].state = State::InFlight;
            }
            // Every connection is busy: try again shortly, without spending an attempt.
            Err(Error::Busy) => self.wait(io, i, self.cfg.backoff_ns),
            Err(e) => self.slots[i].state = State::Finished(Outcome::Fatal(Fatal::Transport(e))),
        }
    }
}
