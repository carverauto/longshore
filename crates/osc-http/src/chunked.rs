// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Decoder for chunked transfer encoding (RFC 9112 section 7.1).
//!
//! Sans-I/O and in place: the caller appends received bytes to a buffer and
//! the decoder moves each chunk's payload down over the framing, so the
//! decoded body ends up contiguous at the front of the same buffer. No
//! allocation, and the result does not depend on how the bytes were split.

use crate::head::HttpError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Reading the hexadecimal chunk size.
    Size {
        value: u64,
        digits: u8,
    },
    /// Whitespace after the size: only an extension or the line end may follow.
    SizeSpace {
        value: u64,
    },
    /// Skipping a chunk extension up to the end of the size line.
    Extension {
        value: u64,
    },
    /// Expecting the LF that ends the size line.
    SizeLf {
        value: u64,
    },
    Data {
        left: u64,
    },
    DataCr,
    DataLf,
    /// Trailer section: `at_line_start` is true right after a CRLF.
    Trailer {
        at_line_start: bool,
    },
    TrailerLf {
        blank: bool,
    },
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunked {
    state: State,
}

impl Default for Chunked {
    fn default() -> Chunked {
        Chunked::new()
    }
}

impl Chunked {
    pub fn new() -> Chunked {
        Chunked {
            state: State::Size {
                value: 0,
                digits: 0,
            },
        }
    }

    /// True once the terminating chunk and trailers have been consumed.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Decode the raw bytes `buf[raw_start..raw_end]`, moving payload to
    /// `buf[*decoded..]` and advancing `*decoded`. `*decoded <= raw_start`
    /// must hold. Returns how many raw bytes were consumed; fewer than given
    /// only when the body is complete.
    pub fn feed(
        &mut self,
        buf: &mut [u8],
        decoded: &mut usize,
        raw_start: usize,
        raw_end: usize,
    ) -> Result<usize, HttpError> {
        debug_assert!(*decoded <= raw_start && raw_start <= raw_end && raw_end <= buf.len());
        let mut at = raw_start;
        while at < raw_end {
            let b = buf[at];
            self.state = match self.state {
                State::Done => break,
                State::Size { value, digits } => match b {
                    b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => {
                        // 16 hex digits is the whole u64; more cannot be a real size.
                        if digits == 16 {
                            return Err(HttpError::BadFraming);
                        }
                        State::Size {
                            value: value << 4
                                | (b as char).to_digit(16).expect("matched a hex digit") as u64,
                            digits: digits + 1,
                        }
                    }
                    _ if digits == 0 => return Err(HttpError::BadFraming),
                    b'\r' => State::SizeLf { value },
                    b';' => State::Extension { value },
                    b' ' | b'\t' => State::SizeSpace { value },
                    _ => return Err(HttpError::BadFraming),
                },
                // "5 ;ext" is tolerated; "5 5" and "5 garbage" are not a size.
                State::SizeSpace { value } => match b {
                    b' ' | b'\t' => State::SizeSpace { value },
                    b';' => State::Extension { value },
                    b'\r' => State::SizeLf { value },
                    _ => return Err(HttpError::BadFraming),
                },
                State::Extension { value } => match b {
                    b'\r' => State::SizeLf { value },
                    b'\n' => return Err(HttpError::BadFraming),
                    _ => State::Extension { value },
                },
                State::SizeLf { value } => match b {
                    b'\n' if value == 0 => State::Trailer {
                        at_line_start: true,
                    },
                    b'\n' => State::Data { left: value },
                    _ => return Err(HttpError::BadFraming),
                },
                State::Data { left } => {
                    // Move as much of this chunk as has arrived, in one step.
                    let n = (left.min((raw_end - at) as u64)) as usize;
                    buf.copy_within(at..at + n, *decoded);
                    *decoded += n;
                    at += n;
                    self.state = if left == n as u64 {
                        State::DataCr
                    } else {
                        State::Data {
                            left: left - n as u64,
                        }
                    };
                    continue;
                }
                State::DataCr if b == b'\r' => State::DataLf,
                State::DataLf if b == b'\n' => State::Size {
                    value: 0,
                    digits: 0,
                },
                State::DataCr | State::DataLf => return Err(HttpError::BadFraming),
                // Trailer fields are skipped; a blank line ends the message.
                State::Trailer { at_line_start } => match b {
                    b'\r' => State::TrailerLf {
                        blank: at_line_start,
                    },
                    b'\n' => return Err(HttpError::BadFraming),
                    _ => State::Trailer {
                        at_line_start: false,
                    },
                },
                State::TrailerLf { blank } => match b {
                    b'\n' if blank => State::Done,
                    b'\n' => State::Trailer {
                        at_line_start: true,
                    },
                    _ => return Err(HttpError::BadFraming),
                },
            };
            at += 1;
        }
        Ok(at - raw_start)
    }
}

#[cfg(test)]
mod tests;
