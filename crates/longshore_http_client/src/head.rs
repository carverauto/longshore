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

/// Header fields a head may hold.
pub const HEADERS_MAX: usize = 32;

/// Why a head was not accepted, or could not be written.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HttpError {
    /// No end of head within the limit.
    HeadTooLarge,
    /// More than [`HEADERS_MAX`] header fields.
    TooManyHeaders,
    /// The request line or status line is malformed.
    BadStartLine,
    /// A header field is malformed.
    BadHeader,
    /// A `Content-Length` is malformed, or two of them differ.
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
    fn get(self, buf: &[u8]) -> &[u8] {
        &buf[self.start as usize..self.start as usize + self.len as usize]
    }
}

/// Header fields common to requests and responses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    head_len: usize,
    content_length: Option<u64>,
    chunked: bool,
    close: bool,
    headers: [(Span, Span); HEADERS_MAX],
    count: usize,
}

impl Fields {
    /// Bytes of the head, including the blank line. The body starts here.
    #[inline]
    #[must_use]
    pub const fn head_len(&self) -> usize {
        self.head_len
    }

    /// The `Content-Length`, if the head carries one.
    #[inline]
    #[must_use]
    pub const fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    /// The body is sent with `Transfer-Encoding: chunked`.
    #[inline]
    #[must_use]
    pub const fn chunked(&self) -> bool {
        self.chunked
    }

    /// The peer will close, or must be closed, after this message.
    #[inline]
    #[must_use]
    pub const fn close(&self) -> bool {
        self.close
    }

    /// First value of a header, by case-insensitive name. `buf` must be the
    /// buffer the head was parsed from.
    pub fn header<'a>(&self, buf: &'a [u8], name: &str) -> Option<&'a [u8]> {
        self.headers[..self.count]
            .iter()
            .find(|(n, _)| n.get(buf).eq_ignore_ascii_case(name.as_bytes()))
            .map(|(_, v)| v.get(buf))
    }

    /// Every value of a header, in the order the lines came, by
    /// case-insensitive name. A header may be sent on several lines, which
    /// means the same as one line with the values joined by commas.
    pub fn headers<'a>(self, buf: &'a [u8], name: &'a str) -> impl Iterator<Item = &'a [u8]> {
        (0..self.count)
            .map(move |i| self.headers[i])
            .filter(move |(n, _)| n.get(buf).eq_ignore_ascii_case(name.as_bytes()))
            .map(move |(_, v)| v.get(buf))
    }
}

/// A response's status and header fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseHead {
    pub(crate) status: u16,
    pub(crate) fields: Fields,
}

impl ResponseHead {
    /// The status code.
    #[inline]
    pub const fn status(self) -> u16 {
        self.status
    }

    /// The header fields.
    #[inline]
    pub const fn fields(self) -> Fields {
        self.fields
    }
}

/// A request's method, target and header fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestHead {
    method: Span,
    target: Span,
    fields: Fields,
}

impl RequestHead {
    /// The request's header fields.
    #[inline]
    #[must_use]
    pub const fn fields(&self) -> &Fields {
        &self.fields
    }

    /// The request method, such as `GET`. `buf` is the buffer the head was
    /// parsed from.
    pub fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.method.get(buf)
    }
    /// The request target. `buf` is the buffer the head was parsed from.
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
        let (line_end, name, value) = header_line(buf, at, end)?;
        if f.count == HEADERS_MAX {
            return Err(HttpError::TooManyHeaders);
        }
        f.headers[f.count] = (name, value);
        f.count += 1;
        keep_alive |= note_framing(&mut f, name.get(buf), value.get(buf))?;
        at = line_end + 2;
    }
    if f.chunked && f.content_length.is_some() {
        return Err(HttpError::BadFraming); // request-smuggling shape; never guess
    }
    f.close |= http10 && !keep_alive;
    Ok(f)
}

/// The header line starting at `at`: where it ends, and its name and its
/// value without surrounding whitespace.
fn header_line(buf: &[u8], at: usize, end: usize) -> Result<(usize, Span, Span), HttpError> {
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
    Ok((
        line_end,
        Span::of(at, at + colon),
        Span::of(at + v0, at + v1),
    ))
}

/// Record what a header says about framing and the connection. Returns
/// whether it asks to keep the connection alive.
fn note_framing(f: &mut Fields, name: &[u8], value: &[u8]) -> Result<bool, HttpError> {
    let mut keep_alive = false;
    if name.eq_ignore_ascii_case(b"content-length") {
        if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
            return Err(HttpError::BadContentLength);
        }
        // Every u64 is a legal length; anything larger is rejected, not wrapped.
        let n = value
            .iter()
            .try_fold(0u64, |n, d| {
                n.checked_mul(10)?.checked_add(u64::from(d - b'0'))
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
    Ok(keep_alive)
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
        .ok_or(HttpError::BadStartLine)?;
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
        .fold(0u16, |n, d| n * 10 + u16::from(d - b'0'));
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
        .ok_or(HttpError::BadStartLine)?;
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

mod writer;
pub use writer::HeadWriter;

#[cfg(test)]
mod tests;
