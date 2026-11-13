// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Sealing outgoing bytes into records and opening received records into the destination.

use super::*;

impl Tls {
    /// Seal as many records as fit in the send buffer and submit them.
    pub(super) fn seal_batch<IO: Io>(&mut self, io: &mut IO, sock: Sock, token: Token) {
        let (src, dst) = io.buf_pair(self.src, self.tx_buf);
        let dst = &mut dst[..self.batch as usize];
        let mut out = 0usize;
        while self.sealed < self.src_len {
            let n = ((self.src_len - self.sealed) as usize).min(FRAGMENT_MAX);
            if out + n + OVERHEAD > dst.len() {
                break;
            }
            let from = (self.src_off + self.sealed) as usize;
            // Reads the caller's bytes, writes the record: the only pass over them.
            match self
                .tx
                .seal(self.tx_type, &src[from..from + n], &mut dst[out..])
            {
                Ok(len) => out += len,
                // The keys have reached their record limit (or are gone).
                // Send what is sealed; `on_send` reports the failure after it.
                Err(e) => {
                    self.tx_error = Some(e);
                    break;
                }
            }
            self.sealed += n as u32;
        }
        (self.cipher_len, self.cipher_sent) = (out as u32, 0);
        io.submit(
            token,
            Op::Send {
                sock,
                buf: self.tx_buf,
                off: 0,
                len: out as u32,
                zero_copy: out as u32 >= self.zero_copy_min,
            },
        )
        .expect("Limits::ops_max must cover one send per connection");
    }

    /// Make space at the end of the receive buffer for more ciphertext.
    pub(super) fn make_room<IO: Io>(&mut self, io: &mut IO) -> Result<(), TlsError> {
        if self.rx_start == self.rx_end {
            (self.rx_start, self.rx_end) = (0, 0);
        } else if self.rx_end == self.cap {
            if self.rx_start == 0 {
                // A single record larger than the buffer.
                return Err(TlsError::BadRecord);
            }
            io.buf(self.rx_buf)
                .copy_within(self.rx_start as usize..self.rx_end as usize, 0);
            (self.rx_start, self.rx_end) = (0, self.rx_end - self.rx_start);
        }
        Ok(())
    }

    /// Deliver plaintext to the destination, or ask `Io` for more ciphertext.
    pub(super) fn pump_rx<IO: Io>(&mut self, io: &mut IO, sock: Sock, token: Token) -> Step {
        let mut produced = 0u32;
        // Content left over from a record that did not fit last time.
        if self.spill_len > 0 {
            let n = self.spill_len.min(self.dst_max);
            let (rx, dst) = io.buf_pair(self.rx_buf, self.dst);
            dst[self.dst_off as usize..(self.dst_off + n) as usize]
                .copy_from_slice(&rx[self.spill_at as usize..(self.spill_at + n) as usize]);
            self.spill_at += n;
            self.spill_len -= n;
            return Step::Done(n as i32);
        }
        while produced < self.dst_max && !self.closed {
            let (rx, dst) = io.buf_pair(self.rx_buf, self.dst);
            let avail = &mut rx[self.rx_start as usize..self.rx_end as usize];
            let len = match record_len(avail) {
                Err(_) => return Step::Tls(TlsError::BadRecord),
                Ok(Some(len)) if avail.len() >= len => len,
                Ok(_) => break,
            };
            let space = (self.dst_max - produced) as usize;
            let at = (self.dst_off + produced) as usize;
            // Opening writes the content and then its type byte, so the
            // destination needs room for both.
            let needed = len - HEADER_LEN - TAG_LEN;
            // The last record of a body usually fits except for its type
            // byte. If the buffer has a byte past the range asked for, the
            // type byte goes there and the byte is put back afterwards.
            // One byte over means the content itself cannot overrun.
            let borrow = needed == space + 1 && at + needed <= dst.len();
            let direct = needed <= space || borrow;
            let lent = borrow.then(|| dst[at + space]);
            let opened = if direct {
                self.rx.open(&avail[..len], &mut dst[at..at + needed])
            } else {
                self.rx.open_in_place(&mut avail[..len])
            };
            if let Some(byte) = lent {
                dst[at + space] = byte;
            }
            let (ctype, n) = match opened {
                Ok(v) => v,
                Err(e) => return Step::Tls(TlsError::Record(e)),
            };
            // An alert is two bytes: level, then description.
            let alert = (ctype == CONTENT_ALERT && n == 2).then(|| {
                if direct {
                    dst[at + 1]
                } else {
                    avail[HEADER_LEN + 1]
                }
            });
            let record_at = self.rx_start;
            self.rx_start += len as u32;
            match ctype {
                CONTENT_APPLICATION_DATA if direct => produced += n as u32,
                CONTENT_APPLICATION_DATA => {
                    // Copy what fits; keep the rest for the next call.
                    let take = n.min(space);
                    let from = record_at as usize + HEADER_LEN;
                    let (rx, dst) = io.buf_pair(self.rx_buf, self.dst);
                    dst[at..at + take].copy_from_slice(&rx[from..from + take]);
                    produced += take as u32;
                    self.spill_at = (from + take) as u32;
                    self.spill_len = (n - take) as u32;
                    // Stop only if content is left over. A record that was
                    // all padding leaves nothing, and the records behind it
                    // must still be read: waiting for the network instead
                    // would hang on a response that has already arrived.
                    if self.spill_len > 0 {
                        break;
                    }
                }
                // Session tickets after the handshake: nothing to do. A peer's
                // KeyUpdate also lands here and is not followed: its next
                // record fails authentication and the connection is dropped.
                CONTENT_HANDSHAKE => {}
                // close_notify is an orderly end of stream; any other alert is a failure.
                CONTENT_ALERT => match alert {
                    Some(0) => self.closed = true,
                    Some(description) => return Step::Tls(TlsError::Alert(description)),
                    None => return Step::Tls(TlsError::BadRecord),
                },
                _ => return Step::Tls(TlsError::BadRecord),
            }
        }
        if produced > 0 {
            return Step::Done(produced as i32);
        }
        if self.closed || self.dst_max == 0 {
            return Step::Done(0);
        }
        if let Err(e) = self.make_room(io) {
            return Step::Tls(e);
        }
        io.submit(
            token,
            Op::Recv {
                sock,
                buf: self.rx_buf,
                off: self.rx_end,
                len: self.cap - self.rx_end,
            },
        )
        .expect("Limits::ops_max must cover one receive per connection");
        Step::Pending
    }
}
