// Copyright 2026 Carver Automation Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License").
// SPDX-License-Identifier: Apache-2.0

//! The operations an [`Io`](super::Io) runs.

use super::{BufId, FileId, FileSlot, OpenFlags, Shut, Sock, Token};
use std::net::SocketAddr;

/// One operation for an [`Io`](super::Io) to run, submitted under a [`Token`]
/// that its completions carry.
#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Open a TCP socket in slot `sock` and connect it. A connect that fails
    /// leaves the slot free; the caller need not close it. A connect that
    /// succeeds completes with 0, or with what [`Segment::decode`](super::Segment::decode) reads
    /// when the backend was asked to measure segment sizes.
    Connect {
        /// The slot to open the socket in.
        sock: Sock,
        /// The address to connect to.
        addr: SocketAddr,
    },
    /// Open a Unix-domain stream socket in slot `sock` and connect it to the
    /// NUL-terminated path in `path_buf[path_off..path_off + path_len]`, copied
    /// at submission time. A connect that fails leaves the slot free.
    ConnectUnix {
        /// The slot to open the socket in.
        sock: Sock,
        /// The pool buffer holding the path.
        path_buf: BufId,
        /// Where the path starts in `path_buf`.
        path_off: u32,
        /// The path's length, its NUL included.
        path_len: u32,
    },
    /// Open a UDP socket in slot `sock` connected to `addr`. `Send` and `Recv`
    /// then carry whole datagrams.
    ConnectUdp {
        /// The slot to open the socket in.
        sock: Sock,
        /// The address to send to and receive from.
        addr: SocketAddr,
    },
    /// Accept one connection from `listener` into slot `sock`.
    Accept {
        /// The listening socket.
        listener: Sock,
        /// The free slot the accepted connection takes.
        sock: Sock,
    },
    /// Accept connections on `listener`, placing each into a free slot of
    /// `Limits::accept_range_start..accept_range_start + accept_range_len` and
    /// reporting the slot index as `result` with `more: true`. Ends with
    /// `-ENFILE` or `-EMFILE` (`more: false`) when the range is full. Counts
    /// toward `Limits::ops_max` until its final completion.
    ArmedAccept {
        /// The listening socket.
        listener: Sock,
    },
    /// Receive bytes on `sock`, taking a buffer from the registered buffer
    /// ring only when bytes arrive. Each completion with bytes carries
    /// `buf: Some(id)` and `result > 0` and remains owned by the caller until
    /// [`Io::give_back`](super::Io::give_back). Ends with `-ENOBUFS` when the ring is empty or `0`
    /// (`buf: None`) on peer EOF. Counts toward `Limits::ops_max` until its
    /// final completion.
    ArmedRecv {
        /// The connected socket.
        sock: Sock,
    },
    /// Receive bytes on `sock` straight into pages of the receive area
    /// ([`Limits::area_pages`](super::Limits::area_pages)), without a copy
    /// by the backend where it can: at most `credit` bytes, which is never
    /// 0. Each completion with bytes is a piece: `result > 0` bytes of the
    /// page [`Completion::buf`](super::Completion::buf) from
    /// [`Completion::off`](super::Completion::off), with `more: true`. A
    /// page can lie behind several pieces, of this receive and of the
    /// socket's next. Each piece stays the caller's until it is returned
    /// with [`Io::give_back_piece`](super::Io::give_back_piece); the page
    /// is not written again before every piece of it is back.
    ///
    /// The last completion has `more: false`: `0` when `credit` bytes were
    /// delivered, and also when the peer finished first, which the caller
    /// tells apart by the bytes it was given; `-ENOSPC` when the backend
    /// had no room to report a piece and `-ENOMEM` when it had no page for
    /// one, both with what was delivered still good and the rest to be
    /// asked for again; or the socket's error. After a turn in which
    /// [`Io::take_ring_full`](super::Io::take_ring_full) says so, a receive
    /// with credit left may stay silent until the peer sends again: cancel
    /// it and arm it again for the rest. Refused with `-ENOBUFS` by a
    /// backend built without an area. Counts toward `Limits::ops_max`
    /// until its last completion.
    ArmedRecvZc {
        /// The connected socket.
        sock: Sock,
        /// The most bytes to deliver before the receive ends; not 0.
        credit: u32,
    },
    /// Send `len` bytes of buffer `buf` starting at `off`. May complete short.
    Send {
        /// The connected socket.
        sock: Sock,
        /// The pool buffer to send from.
        buf: BufId,
        /// Where the bytes start in `buf`.
        off: u32,
        /// How many bytes to send.
        len: u32,
        /// Send straight from the buffer, where the backend can. The operation
        /// then completes only once the kernel has released the buffer.
        zero_copy: bool,
    },
    /// Send `len` bytes of buffer `buf` starting at `off` straight from the
    /// buffer, where the backend can, and say when the bytes were taken
    /// apart from when the buffer is free again. May be taken short.
    ///
    /// A backend that sends from the buffer reports twice under the
    /// operation's token: a first completion with `more: true` and the
    /// bytes taken as soon as they were taken, so that the next send on
    /// the socket can follow at once, and a last completion with
    /// `more: false` and the same result once the backend no longer reads
    /// the buffer. Over a network that is when the peer has acknowledged
    /// the bytes. A backend that copies, and a send that fails before
    /// anything was taken, report once, with `more: false`.
    ///
    /// The bytes sent must not be written before the last completion;
    /// other bytes of the same buffer may be. The last completions of
    /// several such sends on one socket come in no fixed order, nor in a
    /// fixed order with the first completions of later sends. A close of
    /// the socket gives each its last completion like any other operation
    /// in flight. Counts toward `Limits::ops_max` until its last
    /// completion.
    ///
    /// [`Op::Send`] with `zero_copy` is the same send reported once, at
    /// the end.
    SendZc {
        /// The connected socket.
        sock: Sock,
        /// The pool buffer to send from.
        buf: BufId,
        /// Where the bytes start in `buf`.
        off: u32,
        /// How many bytes to send.
        len: u32,
    },
    /// Receive up to `len` bytes into buffer `buf` at `off`. 0 means end of stream.
    Recv {
        /// The connected socket.
        sock: Sock,
        /// The pool buffer to receive into.
        buf: BufId,
        /// Where in `buf` the bytes go.
        off: u32,
        /// The most bytes to receive.
        len: u32,
    },
    /// Like `Recv`, but never waits: completes with the bytes already
    /// received, 0 if the peer has closed, or `-EAGAIN` if nothing has
    /// arrived. Used to check an idle connection before it is reused.
    RecvNow {
        /// The connected socket.
        sock: Sock,
        /// The pool buffer to receive into.
        buf: BufId,
        /// Where in `buf` the bytes go.
        off: u32,
        /// The most bytes to receive.
        len: u32,
    },
    /// Shut down one or both directions of connected stream socket `sock`,
    /// leaving its slot open. After the write side is shut down the peer
    /// reads end of stream once it has read everything sent; after the read
    /// side is shut down, receives on `sock` complete with 0. Completes with
    /// 0, or a negated errno (`-ENOTCONN` on a socket that is not connected).
    Shutdown {
        /// The connected socket.
        sock: Sock,
        /// Which direction to end.
        how: Shut,
    },
    /// Close socket slot `sock`. Gives every other operation in flight on
    /// `sock` (including an [`Op::Accept`] waiting on a closed listener) its
    /// final completion without waiting for the peer, in no fixed order
    /// relative to the close's own completion. Closing a slot that holds no
    /// socket still completes once.
    Close {
        /// The slot to close.
        sock: Sock,
    },
    /// Wait until the backend's waker has been signalled from any thread.
    /// Completes with 0. Signals that arrive while no `AwaitWake` is in
    /// flight are kept, so the next one completes at once. Requires
    /// [`Io::waker`](super::Io::waker) to have been called.
    AwaitWake,
    /// Read a registered file from its start into buffer `buf` at `off`, up
    /// to `len` bytes. Completes with the number of bytes read; the file is
    /// opened and closed around the read, so a replaced file is seen.
    ReadFile {
        /// The file, as [`Io::register_file`](super::Io::register_file) named
        /// it.
        file: FileId,
        /// The pool buffer to read into.
        buf: BufId,
        /// Where in `buf` the bytes go.
        off: u32,
        /// The most bytes to read.
        len: u32,
    },
    /// Completes with 0 after `nanos`.
    Timeout {
        /// How long to wait, in nanoseconds.
        nanos: u64,
    },
    /// Ask for the in-flight operation submitted with `target` at the moment
    /// this cancel is submitted to stop. An operation submitted later under
    /// the same token or already finished is not matched. The target then
    /// completes once, normally with `-ECANCELED`, or with its real result if
    /// it finished first. The cancel itself completes with 0 if found, or a
    /// negative errno if no such operation was in flight.
    Cancel {
        /// The token the operation to stop was submitted with.
        target: Token,
    },
    /// Open the NUL-terminated path in `path_buf[path_off..path_off + path_len]`
    /// into fixed file slot `slot`. Completes with 0 or a negative errno.
    FileOpen {
        /// The free file slot the opened file takes.
        slot: FileSlot,
        /// The pool buffer holding the path.
        path_buf: BufId,
        /// Where the path starts in `path_buf`.
        path_off: u32,
        /// The path's length, its NUL included.
        path_len: u32,
        /// How to open the file.
        flags: OpenFlags,
    },
    /// Close the file in `slot`, freeing the slot on completion.
    FileClose {
        /// The slot to close.
        slot: FileSlot,
    },
    /// Read up to `len` bytes at `file_off` from `slot` into `buf[off..off + len]`.
    FileRead {
        /// The open file.
        slot: FileSlot,
        /// The pool buffer to read into.
        buf: BufId,
        /// Where in `buf` the bytes go.
        off: u32,
        /// The most bytes to read.
        len: u32,
        /// Where in the file to read from.
        file_off: u64,
    },
    /// Write up to `len` bytes from `buf[off..off + len]` to `slot` at `file_off`.
    FileWrite {
        /// The open file.
        slot: FileSlot,
        /// The pool buffer to write from.
        buf: BufId,
        /// Where the bytes start in `buf`.
        off: u32,
        /// The most bytes to write.
        len: u32,
        /// Where in the file to write to.
        file_off: u64,
    },
    /// Write `len` bytes to `slot` at `file_off` as one write gathered from
    /// `count` spans of the pool, in order. The list of spans lies in
    /// `list` from `list_off`, twelve bytes a span: a buffer's id, an
    /// offset in it and a length, each a little-endian `u32`; `len` is the
    /// sum of their lengths. A span may be of any buffer class, a page of
    /// the receive area among them. For a file opened for direct I/O
    /// every span is whole blocks of a block-aligned buffer or of a page,
    /// from a block boundary in it. The spans' memory, and the list
    /// itself, are read until the write completes, not only when it is
    /// submitted: neither may change before (a backend may read the list
    /// at submission, as the ring does, or when it writes, as the portable
    /// backend and the simulator do). As many gathered writes may be in
    /// flight as `Limits::gather_ops`; one more is refused with
    /// `-ENOBUFS`. Completes like [`Op::FileWrite`], possibly short.
    FileWriteSpans {
        /// The open file.
        slot: FileSlot,
        /// The pool buffer the list of spans lies in.
        list: BufId,
        /// Where the list starts in `list`.
        list_off: u32,
        /// How many spans it has; not 0.
        count: u32,
        /// The bytes of all spans together.
        len: u32,
        /// Where in the file to write to.
        file_off: u64,
    },
    /// Flush dirty data (and metadata unless `datasync`) for `slot`.
    FileSync {
        /// The open file.
        slot: FileSlot,
        /// Leave out metadata that reading the data back does not need.
        datasync: bool,
    },
    /// Reserve space for `[file_off, file_off + len)` on `slot`.
    FileReserve {
        /// The open file.
        slot: FileSlot,
        /// Where the range starts in the file.
        file_off: u64,
        /// The range's length in bytes.
        len: u64,
        /// Leave the file's size as it is even when the range ends past it.
        keep_size: bool,
    },
    /// Deallocate `[file_off, file_off + len)` on `slot`, leaving file size unchanged.
    FilePunch {
        /// The open file.
        slot: FileSlot,
        /// Where the range starts in the file.
        file_off: u64,
        /// The range's length in bytes.
        len: u64,
    },
    /// Read file status by path into `out_buf[out_off..]` in 256-byte Linux `statx` layout.
    FileStatus {
        /// The pool buffer holding the path.
        path_buf: BufId,
        /// Where the path starts in `path_buf`.
        path_off: u32,
        /// The path's length, its NUL included.
        path_len: u32,
        /// The pool buffer the status is written to.
        out_buf: BufId,
        /// Where in `out_buf` the status goes.
        out_off: u32,
    },
    /// Rename a path (overwriting an existing non-directory target).
    FileRename {
        /// The pool buffer holding the present path.
        old_buf: BufId,
        /// Where the present path starts in `old_buf`.
        old_off: u32,
        /// The present path's length, its NUL included.
        old_len: u32,
        /// The pool buffer holding the new path.
        new_buf: BufId,
        /// Where the new path starts in `new_buf`.
        new_off: u32,
        /// The new path's length, its NUL included.
        new_len: u32,
    },
    /// Unlink a non-directory path.
    FileUnlink {
        /// The pool buffer holding the path.
        path_buf: BufId,
        /// Where the path starts in `path_buf`.
        path_off: u32,
        /// The path's length, its NUL included.
        path_len: u32,
    },
    /// Create a directory at the given path.
    FileMkdir {
        /// The pool buffer holding the path.
        path_buf: BufId,
        /// Where the path starts in `path_buf`.
        path_off: u32,
        /// The path's length, its NUL included.
        path_len: u32,
    },
}
