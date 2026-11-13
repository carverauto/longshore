// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Driving the TLS handshake over the stream until traffic keys are installed.

use super::*;

impl Stream {
    // ---- handshake -------------------------------------------------------

    /// Begin a TLS handshake on a connected socket. Drive it by passing
    /// send and receive completions to [`Stream::on_handshake`].
    pub fn start_handshake<IO: Io>(&mut self, io: &mut IO, handshake: Handshake) -> Step {
        let t = self
            .tls
            .as_mut()
            .expect("start_handshake needs TLS buffers");
        t.handshake = Some(handshake);
        t.active = true;
        (t.rx_start, t.rx_end, t.spill_len, t.closed) = (0, 0, 0, false);
        self.pump_handshake(io)
    }

    pub(super) fn pump_handshake<IO: Io>(&mut self, io: &mut IO) -> Step {
        let (sock, send_token, recv_token) = (self.sock, self.send_token, self.recv_token);
        let t = self.tls.as_mut().expect("handshake state");
        loop {
            let hs = t.handshake.as_mut().expect("handshake in progress");
            let n = hs.write(&mut io.buf(t.tx_buf)[..t.cap as usize]);
            if n > 0 {
                (t.cipher_len, t.cipher_sent) = (n as u32, 0);
                Self::submit(
                    io,
                    send_token,
                    Op::Send {
                        sock,
                        buf: t.tx_buf,
                        off: 0,
                        len: n as u32,
                        zero_copy: false,
                    },
                );
                return Step::Pending;
            }
            if hs.is_done() {
                let hs = t.handshake.take().expect("checked above");
                return match hs.finish(&mut t.tx, &mut t.rx) {
                    Ok(()) => Step::Done(0),
                    Err(e) => Step::Tls(e),
                };
            }
            let used = match hs.read(&io.buf(t.rx_buf)[t.rx_start as usize..t.rx_end as usize]) {
                Ok(n) => n as u32,
                Err(e) => return Step::Tls(e),
            };
            t.rx_start += used;
            if used > 0 {
                continue;
            }
            // Nothing to send and no whole record to read: wait for bytes.
            if let Err(e) = t.make_room(io) {
                return Step::Tls(e);
            }
            Self::submit(
                io,
                recv_token,
                Op::Recv {
                    sock,
                    buf: t.rx_buf,
                    off: t.rx_end,
                    len: t.cap - t.rx_end,
                },
            );
            return Step::Pending;
        }
    }

    /// Feed a send (`sent = true`) or receive completion during the
    /// handshake. `Done(0)` means the handshake completed; a negative
    /// `Done` or `Done(0)` on a receive of zero bytes is reported as given.
    pub fn on_handshake<IO: Io>(&mut self, io: &mut IO, sent: bool, result: i32) -> Step {
        let (sock, send_token) = (self.sock, self.send_token);
        let t = self.tls.as_mut().expect("handshake state");
        if result <= 0 {
            // A negative errno, or the peer closed mid-handshake.
            return Step::Done(if result == 0 {
                -osc_io::ECONNRESET
            } else {
                result
            });
        }
        if sent {
            t.cipher_sent += result as u32;
            if t.cipher_sent < t.cipher_len {
                Self::submit(
                    io,
                    send_token,
                    Op::Send {
                        sock,
                        buf: t.tx_buf,
                        off: t.cipher_sent,
                        len: t.cipher_len - t.cipher_sent,
                        zero_copy: false,
                    },
                );
                return Step::Pending;
            }
        } else {
            t.rx_end += result as u32;
        }
        self.pump_handshake(io)
    }
}
