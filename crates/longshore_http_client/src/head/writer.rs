// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Writing request and response heads into a fixed buffer.

use super::{is_token, HttpError};

/// Writes a request (or, for the test server, a response) head into a fixed
/// buffer. Overflow and illegal bytes are remembered and reported by `finish`.
pub struct HeadWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
    bad: bool,
}

impl<'a> HeadWriter<'a> {
    /// `METHOD target HTTP/1.1`.
    pub fn request(buf: &'a mut [u8], method: &str, target: &str) -> HeadWriter<'a> {
        let mut w = HeadWriter {
            buf,
            len: 0,
            bad: false,
        };
        w.bad |= method.is_empty()
            || !method.bytes().all(is_token)
            || target.is_empty()
            || target.bytes().any(|b| b <= 0x20 || b == 0x7f);
        w.put(method.as_bytes());
        w.put(b" ");
        w.put(target.as_bytes());
        w.put(b" HTTP/1.1\r\n");
        w
    }

    /// `HTTP/1.1 status reason`.
    pub fn response(buf: &'a mut [u8], status: u16, reason: &str) -> HeadWriter<'a> {
        let mut w = HeadWriter {
            buf,
            len: 0,
            bad: false,
        };
        w.bad |= !(100..1000).contains(&status) || reason.bytes().any(|b| b < 0x20 || b == 0x7f);
        w.put(b"HTTP/1.1 ");
        w.put_u64(u64::from(status));
        w.put(b" ");
        w.put(reason.as_bytes());
        w.put(b"\r\n");
        w
    }

    fn put(&mut self, b: &[u8]) {
        match self.buf.get_mut(self.len..self.len + b.len()) {
            Some(dst) => {
                dst.copy_from_slice(b);
                self.len += b.len();
            }
            None => self.bad = true,
        }
    }

    fn put_u64(&mut self, mut v: u64) {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.put(&digits[i..]);
    }

    fn name(&mut self, name: &str) {
        self.bad |= name.is_empty() || !name.bytes().all(is_token);
        self.put(name.as_bytes());
        self.put(b": ");
    }

    /// Add a header field. A name or value with bytes a field may not hold
    /// makes [`HeadWriter::finish`] fail.
    pub fn header(&mut self, name: &str, value: &[u8]) -> &mut Self {
        // CR or LF here would let a value inject headers or a second request.
        self.bad |= value.iter().any(|b| *b < 0x20 && *b != b'\t' || *b == 0x7f);
        self.name(name);
        self.put(value);
        self.put(b"\r\n");
        self
    }

    /// Add a header field whose value is a number.
    pub fn header_u64(&mut self, name: &str, value: u64) -> &mut Self {
        self.name(name);
        self.put_u64(value);
        self.put(b"\r\n");
        self
    }

    /// Terminate the head. Returns its length.
    pub fn finish(&mut self) -> Result<usize, HttpError> {
        self.put(b"\r\n");
        if self.bad {
            Err(HttpError::BadRequest)
        } else {
            Ok(self.len)
        }
    }
}
