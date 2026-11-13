// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Request and response heads (RFC 9112), strict by design.
//!
//! Parsing is stateless over the bytes received so far: call again with a
//! longer slice when it returns `Ok(None)`. The result does not depend on how
//! the bytes were split across reads. Headers are recorded as offsets into
//! the caller's buffer; nothing is copied.

pub const HEADERS_MAX: usize = 32;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HttpError {
    /// No end of head within the limit.
    HeadTooLarge,
    TooManyHeaders,
    BadStartLine,
    BadHeader,
    BadContentLength,
    /// Both Content-Length and Transfer-Encoding, or an encoding other than chunked.
    BadFraming,
    /// A request head field contains CR, LF or NUL, or the head does not fit.
    BadRequest,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Span {
    start: u16,
    len: u16,
}

impl Span {
    fn of(start: usize, end: usize) -> Span {
        Span {
            start: start as u16,
            len: (end - start) as u16,
        }
    }
    fn get<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.start as usize..self.start as usize + self.len as usize]
    }
}

/// Header fields common to requests and responses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    /// Bytes of the head, including the blank line. The body starts here.
    pub head_len: usize,
    pub content_length: Option<u64>,
    pub chunked: bool,
    /// The peer will close, or must be closed, after this message.
    pub close: bool,
    headers: [(Span, Span); HEADERS_MAX],
    count: usize,
}

impl Fields {
    /// First value of a header, by case-insensitive name. `buf` must be the
    /// buffer the head was parsed from.
    pub fn header<'a>(&self, buf: &'a [u8], name: &str) -> Option<&'a [u8]> {
        self.headers[..self.count]
            .iter()
            .find(|(n, _)| n.get(buf).eq_ignore_ascii_case(name.as_bytes()))
            .map(|(_, v)| v.get(buf))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseHead {
    pub status: u16,
    pub fields: Fields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestHead {
    method: Span,
    target: Span,
    pub fields: Fields,
}

impl RequestHead {
    pub fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.method.get(buf)
    }
    pub fn target<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.target.get(buf)
    }
}

fn is_token(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Position just past the blank line, if the head is complete within `limit`.
fn head_end(buf: &[u8], limit: usize) -> Result<Option<usize>, HttpError> {
    let window = &buf[..buf.len().min(limit)];
    match window.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => Ok(Some(i + 4)),
        None if buf.len() >= limit => Err(HttpError::HeadTooLarge),
        None => Ok(None),
    }
}

/// Parse the header lines in `buf[start..end]` (`end` is past the blank line).
fn fields(buf: &[u8], start: usize, end: usize, http10: bool) -> Result<Fields, HttpError> {
    let mut f = Fields {
        head_len: end,
        content_length: None,
        chunked: false,
        close: false,
        headers: [(Span::default(), Span::default()); HEADERS_MAX],
        count: 0,
    };
    let mut keep_alive = false;
    let mut at = start;
    while at < end - 2 {
        let line_end = at
            + buf[at..end]
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or(HttpError::BadHeader)?;
        let line = &buf[at..line_end];
        let colon = line
            .iter()
            .position(|b| *b == b':')
            .ok_or(HttpError::BadHeader)?;
        // No whitespace before the colon, no folded lines, no control bytes.
        if colon == 0
            || !line[..colon].iter().all(|b| is_token(*b))
            || line[colon + 1..]
                .iter()
                .any(|b| *b < 0x20 && *b != b'\t' || *b == 0x7f)
        {
            return Err(HttpError::BadHeader);
        }
        let mut v0 = colon + 1;
        let mut v1 = line.len();
        while v0 < v1 && matches!(line[v0], b' ' | b'\t') {
            v0 += 1;
        }
        while v1 > v0 && matches!(line[v1 - 1], b' ' | b'\t') {
            v1 -= 1;
        }
        if f.count == HEADERS_MAX {
            return Err(HttpError::TooManyHeaders);
        }
        let (name, value) = (&line[..colon], &line[v0..v1]);
        f.headers[f.count] = (Span::of(at, at + colon), Span::of(at + v0, at + v1));
        f.count += 1;
        if name.eq_ignore_ascii_case(b"content-length") {
            if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                return Err(HttpError::BadContentLength);
            }
            // Every u64 is a legal length; anything larger is rejected, not wrapped.
            let n = value
                .iter()
                .try_fold(0u64, |n, d| {
                    n.checked_mul(10)?.checked_add((d - b'0') as u64)
                })
                .ok_or(HttpError::BadContentLength)?;
            if f.content_length.is_some_and(|prev| prev != n) {
                return Err(HttpError::BadContentLength);
            }
            f.content_length = Some(n);
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            if !value.eq_ignore_ascii_case(b"chunked") || f.chunked {
                return Err(HttpError::BadFraming);
            }
            f.chunked = true;
        } else if name.eq_ignore_ascii_case(b"connection") {
            for token in value.split(|b| *b == b',') {
                let t = token.trim_ascii();
                f.close |= t.eq_ignore_ascii_case(b"close");
                keep_alive |= t.eq_ignore_ascii_case(b"keep-alive");
            }
        }
        at = line_end + 2;
    }
    if f.chunked && f.content_length.is_some() {
        return Err(HttpError::BadFraming); // request-smuggling shape; never guess
    }
    f.close |= http10 && !keep_alive;
    Ok(f)
}

/// `HTTP/1.0` or `HTTP/1.1` at the start of `s`; returns whether it is 1.0.
fn version(s: &[u8]) -> Result<bool, HttpError> {
    match s {
        b"HTTP/1.1" => Ok(false),
        b"HTTP/1.0" => Ok(true),
        _ => Err(HttpError::BadStartLine),
    }
}

/// Parse a response head from the bytes received so far. `Ok(None)` means
/// more bytes are needed. `limit` is the largest head accepted.
pub fn parse_response(buf: &[u8], limit: usize) -> Result<Option<ResponseHead>, HttpError> {
    debug_assert!(limit <= u16::MAX as usize);
    // Reject garbage on its first bytes, before any other check, so the
    // verdict is the same however much of it has arrived.
    let probe = &buf[..buf.len().min(7)];
    if !(b"HTTP/1.".starts_with(probe) || probe.starts_with(b"HTTP/1.")) {
        return Err(HttpError::BadStartLine);
    }
    let Some(end) = head_end(buf, limit)? else {
        return Ok(None);
    };
    let line_end = buf[..end]
        .windows(2)
        .position(|w| w == b"\r\n")
        .expect("head ends with CRLF CRLF");
    let line = &buf[..line_end];
    // "HTTP/1.x SSS" then end of line or a space and the reason phrase.
    if line.len() < 12
        || line[8] != b' '
        || !(line.len() == 12 || line[12] == b' ')
        || !line[9..12].iter().all(u8::is_ascii_digit)
        || line[12..].iter().any(|b| *b < 0x20 && *b != b'\t')
    {
        return Err(HttpError::BadStartLine);
    }
    let http10 = version(&line[..8])?;
    let status = line[9..12]
        .iter()
        .fold(0u16, |n, d| n * 10 + (d - b'0') as u16);
    if status < 100 {
        return Err(HttpError::BadStartLine);
    }
    Ok(Some(ResponseHead {
        status,
        fields: fields(buf, line_end + 2, end, http10)?,
    }))
}

/// Parse a request head. Used by the in-tree test server.
pub fn parse_request(buf: &[u8], limit: usize) -> Result<Option<RequestHead>, HttpError> {
    debug_assert!(limit <= u16::MAX as usize);
    let Some(end) = head_end(buf, limit)? else {
        return Ok(None);
    };
    let line_end = buf[..end]
        .windows(2)
        .position(|w| w == b"\r\n")
        .expect("head ends with CRLF CRLF");
    let line = &buf[..line_end];
    let sp1 = line
        .iter()
        .position(|b| *b == b' ')
        .ok_or(HttpError::BadStartLine)?;
    let sp2 = sp1
        + 1
        + line[sp1 + 1..]
            .iter()
            .position(|b| *b == b' ')
            .ok_or(HttpError::BadStartLine)?;
    if sp1 == 0
        || sp2 == sp1 + 1
        || !line[..sp1].iter().all(|b| is_token(*b))
        || line[sp1 + 1..sp2].iter().any(|b| *b <= 0x20 || *b == 0x7f)
    {
        return Err(HttpError::BadStartLine);
    }
    let http10 = version(&line[sp2 + 1..])?;
    Ok(Some(RequestHead {
        method: Span::of(0, sp1),
        target: Span::of(sp1 + 1, sp2),
        fields: fields(buf, line_end + 2, end, http10)?,
    }))
}

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
        w.put_u64(status as u64);
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

    pub fn header(&mut self, name: &str, value: &[u8]) -> &mut Self {
        // CR or LF here would let a value inject headers or a second request.
        self.bad |= value.iter().any(|b| *b < 0x20 && *b != b'\t' || *b == 0x7f);
        self.name(name);
        self.put(value);
        self.put(b"\r\n");
        self
    }

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

#[cfg(test)]
mod tests;
