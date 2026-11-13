// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! Carrying out one request against the object table, with scripted misbehaviour.

use super::*;

impl Server {
    /// A complete request has been read; its body, if any, is in the body buffer.
    pub(super) fn handle<IO: Io>(&mut self, io: &mut IO, i: usize) {
        self.requests += 1;
        let behavior = if self.script_len > 0 {
            let b = self.script[self.script_head];
            self.script_head = (self.script_head + 1) % SCRIPT_MAX;
            self.script_len -= 1;
            b
        } else {
            Behavior::Normal
        };
        let c = self.conns[i];
        let target = &c.target[..c.target_len];
        let method = &c.method[..c.method_len];
        match behavior {
            Behavior::CloseBeforeResponse => return self.close(io, i),
            Behavior::TlsAlert(description) => {
                // Sent like a two-byte head; the connection closes after it.
                let c = &mut self.conns[i];
                (c.head_len, c.body_len, c.close_after) = (2, 0, true);
                c.state = State::SendHead { off: 0 };
                let buf = self.head_buf(i);
                return self.streams[i].send_alert(io, buf, 0, 2, description);
            }
            Behavior::Stall => {
                // Wait for the client to give up; a read notices it closing.
                self.conns[i].state = State::Stalled;
                return self.recv_into(io, i, self.head_buf(i), 0, 1);
            }
            Behavior::GarbageHead => {
                let junk = b"\x00\x01 this is not HTTP \xff\xfe\r\n\r\n";
                io.buf(self.head_buf(i))[..junk.len()].copy_from_slice(junk);
                return self.respond(io, i, junk.len(), 0, true);
            }
            Behavior::EndlessHead => {
                let n = self.cfg.head_max as usize;
                let buf = &mut io.buf(self.head_buf(i))[..n];
                buf.fill(b'a');
                buf[..22].copy_from_slice(b"HTTP/1.1 200 OK\r\nX-A: ");
                return self.respond(io, i, n, 0, true);
            }
            Behavior::Raw(bytes) => {
                io.buf(self.head_buf(i))[..bytes.len()].copy_from_slice(bytes);
                return self.respond(io, i, bytes.len(), 0, true);
            }
            Behavior::Status(s) => return self.status(io, i, s, "Scripted", 0, 0, false),
            Behavior::StatusWithBody(s, n) => {
                io.buf(self.body_buf(i))[..n as usize].fill(b'e');
                return self.status(io, i, s, "Scripted", n as u64, n, false);
            }
            Behavior::ErrorDocument(s, code) => {
                return self.error_document(io, i, s, code);
            }
            Behavior::Chunked => {
                // The server closes after this response, and says so.
                let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n0\r\n\r\n";
                io.buf(self.head_buf(i))[..head.len()].copy_from_slice(head);
                return self.respond(io, i, head.len(), 0, true);
            }
            Behavior::BadChunk => {
                let head =
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\noops\r\n0\r\n\r\n";
                io.buf(self.head_buf(i))[..head.len()].copy_from_slice(head);
                return self.respond(io, i, head.len(), 0, true);
            }
            Behavior::ChunkedBody(chunk) => {
                let Some(slot) = self
                    .objects
                    .iter()
                    .position(|o| o.used && &o.target[..o.target_len] == target)
                else {
                    return self.status(io, i, 404, "Not Found", 0, 0, false);
                };
                // Encode the object into the body buffer, chunk by chunk.
                let o = &self.objects[slot];
                let body = io.buf(BufId(self.cfg.first_buf + i as u32 * 2 + 1));
                let (mut n, mut sent) = (0usize, 0usize);
                let hex = |v: usize, out: &mut [u8]| -> usize {
                    let digits = format_hex(v);
                    let len = digits.1;
                    out[..len].copy_from_slice(&digits.0[16 - len..]);
                    len
                };
                while sent < o.len as usize {
                    let k = (chunk.max(1) as usize).min(o.len as usize - sent);
                    n += hex(k, &mut body[n..]);
                    body[n..n + 2].copy_from_slice(b"\r\n");
                    body[n + 2..n + 2 + k].copy_from_slice(&o.data[sent..sent + k]);
                    body[n + 2 + k..n + 4 + k].copy_from_slice(b"\r\n");
                    n += 4 + k;
                    sent += k;
                }
                body[n..n + 5].copy_from_slice(b"0\r\n\r\n");
                n += 5;
                let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
                io.buf(self.head_buf(i))[..head.len()].copy_from_slice(head);
                return self.respond(io, i, head.len(), n as u32, false);
            }
            Behavior::Normal | Behavior::TruncateBody | Behavior::ApplyThenClose => {}
        }
        if !c.authorized {
            let doc = b"<?xml version=\"1.0\"?><Error><Code>InvalidAccessKeyId</Code></Error>";
            io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
            return self.status(
                io,
                i,
                403,
                "Forbidden",
                doc.len() as u64,
                doc.len() as u32,
                false,
            );
        }
        let is_listing = target.starts_with(b"/?")
            && target[2..]
                .split(|b| *b == b'&')
                .any(|pair| pair == b"list-type=2");
        if method == b"POST" && target == b"/?delete=" {
            return self.erase_many(io, i);
        }
        if method == b"PUT" && c.copy_source_len > 0 {
            return self.copy(io, i, behavior == Behavior::ApplyThenClose);
        }
        if method == b"GET" && is_listing {
            return self.list(io, i);
        }
        if self.multipart(io, i, behavior == Behavior::ApplyThenClose) {
            return;
        }
        let truncate = behavior == Behavior::TruncateBody;
        let found = self
            .objects
            .iter()
            .position(|o| o.used && &o.target[..o.target_len] == target);
        match method {
            b"PUT" => {
                if found.is_some() && c.if_none_match {
                    return self.status(io, i, 412, "Precondition Failed", 0, 0, false);
                }
                let Some(slot) = found.or_else(|| self.objects.iter().position(|o| !o.used)) else {
                    return self.status(io, i, 507, "Insufficient Storage", 0, 0, false);
                };
                let len = c.request_len as usize;
                // Enforce the claimed checksum the way a real store does.
                if let (Some(sum), Some(claimed)) = (self.cfg.checksum, c.claimed) {
                    if sum(&io.buf(self.body_buf(i))[..len]) != claimed {
                        // Each store names this error differently; answer as the one whose header was used.
                        let doc: &[u8] = if claimed[8..] == [0; 4] {
                            b"<?xml version='1.0' encoding='UTF-8'?><Error><Code>CrcMismatch</Code><Message>The CRC32C you specified did not match what we computed.</Message></Error>"
                        } else {
                            b"<?xml version=\"1.0\"?><Error><Code>BadDigest</Code><Message>checksum mismatch</Message></Error>"
                        };
                        io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
                        return self.status(
                            io,
                            i,
                            400,
                            "Bad Request",
                            doc.len() as u64,
                            doc.len() as u32,
                            false,
                        );
                    }
                }
                let o = &mut self.objects[slot];
                o.checksum = c.claimed.filter(|_| self.cfg.checksum.is_some());
                o.data[..len]
                    .copy_from_slice(&io.buf(BufId(self.cfg.first_buf + i as u32 * 2 + 1))[..len]); // body buffer
                o.target[..c.target_len].copy_from_slice(&c.target[..c.target_len]);
                o.target_len = c.target_len;
                o.len = len as u32;
                o.used = true;
                o.describing = c.describing;
                o.describing_len = c.describing_len;
                if behavior == Behavior::ApplyThenClose {
                    return self.close(io, i);
                }
                // A stored part is answered with its tag.
                if target.windows(12).any(|w| w == b"?partNumber=") {
                    self.conns[i].reply_tag = Some(multipart::tag(&self.objects[slot].data[..len]));
                }
                self.status(io, i, 200, "OK", 0, 0, false)
            }
            b"GET" | b"HEAD" => {
                let len = match found {
                    Some(slot) => {
                        self.conns[i].reply_object = Some(slot as u32);
                        let o = &self.objects[slot];
                        // A satisfiable range is served as 206 with just those bytes.
                        if let Some((a, b)) = c
                            .range
                            .filter(|(a, b)| a <= b && *a < o.len && method == b"GET")
                        {
                            let b = b.min(o.len - 1);
                            let n = (b - a + 1) as usize;
                            io.buf(self.body_buf(i))[..n]
                                .copy_from_slice(&o.data[a as usize..a as usize + n]);
                            return self.status(
                                io,
                                i,
                                206,
                                "Partial Content",
                                n as u64,
                                n as u32,
                                false,
                            );
                        }
                        io.buf(self.body_buf(i))[..o.len as usize]
                            .copy_from_slice(&o.data[..o.len as usize]);
                        // S3 returns a checksum only when asked; Google Cloud Storage always does.
                        self.conns[i].reply_checksum = o
                            .checksum
                            .filter(|sum| c.checksum_mode || sum[8..] == [0; 4]);
                        o.len
                    }
                    None => return self.status(io, i, 404, "Not Found", 0, 0, false),
                };
                let send = if method == b"HEAD" {
                    0
                } else if truncate {
                    len / 2
                } else {
                    len
                };
                self.status(io, i, 200, "OK", len as u64, send, truncate)
            }
            b"DELETE" => {
                match found {
                    Some(slot) => self.objects[slot].used = false,
                    // Google Cloud Storage refuses to delete what is not there; S3 does not.
                    None if c.bearer => {
                        let doc = b"<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>";
                        io.buf(self.body_buf(i))[..doc.len()].copy_from_slice(doc);
                        return self.status(
                            io,
                            i,
                            404,
                            "Not Found",
                            doc.len() as u64,
                            doc.len() as u32,
                            false,
                        );
                    }
                    None => {}
                }
                if behavior == Behavior::ApplyThenClose {
                    return self.close(io, i);
                }
                self.status(io, i, 204, "No Content", 0, 0, false)
            }
            _ => self.status(io, i, 405, "Method Not Allowed", 0, 0, true),
        }
    }
}
