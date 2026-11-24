## Context
longshore has the pieces below a TCP layer: the `Io` trait (connect, accept,
armed accept, receive from a buffer ring with `give_back`, zero-copy send,
timeouts, cancel, cross-thread wake, `TCP_NODELAY`, `SO_KEEPALIVE`, Unix
sockets) and `longshore_stream` (one connection's plain or TLS byte stream).
What sits above them today is HTTP only.

The first user is UltraGraphDB (survey of its networking, 2026-11-24):
- A binary protocol with a fixed 256-byte header whose u32 at offset 40 is
  the frame's whole length; frames up to 1 MiB; reads in two fills (header,
  then the rest). Streams are multiplexed and pipelined inside the protocol.
- Role links (compute to partitions and system group, partition to
  partition): long-lived, redialled after a close; dial timeout 10 s.
- A client endpoint of up to 4,096 connections per compute node, refusing
  the next with an error frame; per-core accept on one shared listener
  (today a dup of one socket); bind retried on `EADDRINUSE` for 5 s; accept
  backoff from 5 ms to 1 s on `EMFILE`.
- Backpressure by not reading: a connection stops reading while its session
  holds 4 MiB of requests; per-stream credit inside the protocol.
- `TCP_NODELAY`, `SO_KEEPALIVE`, peer address, half-close, TCP and Unix
  sockets, TLS 1.3 on the client endpoint, graceful drain on SIGTERM.
- Today: a fresh `Vec` per sent frame, one write per frame, every parked
  request copied, a 1 MiB buffer allocated per new connection.

smoltcp (a TCP/IP stack without a heap) shows an API shape that fits these
rules: storage handed in at construction, handles into a fixed set, closures
that read and write the socket's buffers in place and report how much they
used, and a stack that says when it next needs polling while the caller
owns the clock. stdx's networking is tokio's `TcpStream` with rustls and
offers nothing specific to io_uring.

## Goals / Non-Goals
- Goals: the fastest TCP over io_uring under longshore's first principles;
  an API that a kernel-bypass backend (change 3) can sit behind unchanged;
  every need above met without a copy of the payload by the crate.
- Non-Goals: an executor, futures or wakers (callers are state machines);
  application protocols (streams, credit, authentication stay in the
  caller); UDP; HTTP (stays in `longshore_http_client`); multi-node
  simulation (stays in UltraGraphDB).

## Decisions

### Layers
```
caller's state machines (UltraGraphDB roles, ...)
  Net<T: Transport>      handles, framing, held frames, send queues,
                         deadlines, reconnect, accept policy, drain
  Transport              backend seam: byte streams over lent buffers
    KernelTransport<IO: Io>    kernel TCP: io_uring | portable | simulator
    (change 3) XdpTransport    AF_XDP + user-space TCP
```
`Net` holds everything that does not depend on how bytes reach the wire, so
a second transport reuses it whole. `Transport` speaks in slots, lent
receive buffers, reserved send space and transport events; it names no
`Op`, `BufId` or `Sock`. `KernelTransport` is written against `Io`, so it
runs on `UringIo`, `PosixIo` and `SimIo` without further work.

### One `Net` per thread
No locks and no `Send` bound on the data path. A program with N cores runs
N `Net`s; messages between them use `longshore_mailbox`. Each core listens
on the same address with `SO_REUSEPORT`, so the kernel spreads connections
across cores without a shared socket (replacing UltraGraphDB's dup).

### Everything sized at build
`Config` fixes: connections and listeners; the frame pool (count × maximum
frame size); the receive buffer ring (count × size); send segments (count ×
size); the deadline table (one entry per connection and listener, so no
heap). `Net::new` allocates and registers it all; nothing allocates after,
checked under `alloc_guard`. The TLS handshake keeps its sanctioned
exception.

### Handles
`Conn { slot: u32, gen: u32 }` and `Listener { slot, gen }` are `Copy`. A
slot's generation rises each time it is freed; an operation with a stale
handle returns `Error::Stale` and touches nothing. (smoltcp's handles are
bare indices; the generation closes the reuse hole.)

### Framing
`Framing::Bytes` passes received bytes through as they come: the caller's
closure sees the contiguous bytes received so far and says how many it
consumed (smoltcp's `recv`). `Framing::Header { header_len, len_at,
len_width, big_endian, len_counts_header, max_frame }` delivers whole
frames: a 4-byte length prefix is `{4, 0, 4, true, false, ..}`;
UltraGraphDB's is `{256, 40, 4, false, true, 1 MiB}`. A length below the
header or above `max_frame` closes the connection with
`Closed::Protocol`.

### Receiving: two modes, chosen per endpoint
- `Rx::Ring` (default for listeners with many connections): one armed
  receive per connection from a shared buffer ring. A frame that lies whole
  inside one ring buffer is lent in place; the buffer goes back to the ring
  when every frame in it is released. A frame that spans buffers, or is
  larger than one, is assembled into a frame from the pool: one copy, for
  that frame only.
- `Rx::Direct` (default for links with large frames): the connection
  receives the header into its own small area, then the body straight
  into a pool frame of the exact size. No copy for any frame; two receives
  per frame.
Change 2's benchmarks set the defaults and the ring buffer size.

### Holding frames
`Event::Frame { conn, frame }` carries a `FrameId`. `net.frame(id)` gives
the bytes; `net.release(id)` returns the memory. Frames may be held across
turns (UltraGraphDB parks requests), so nothing needs copying out. Each
connection has a held-bytes budget; at the budget the connection stops
reading (the armed receive is cancelled, or no receive is posted) and
resumes when releases bring it under. Nothing is refused.

### Sending
`net.send_frame(conn, len, |buf| ..)` reserves `len` bytes in the
connection's send queue, which is a chain of send segments taken from a
shared pool of registered buffers, and lets the caller serialise into them
in place. Frames queued back to back in a segment go out as one send;
segments at or above a size go zero-copy (`SEND_ZC`). A full queue or an
empty segment pool returns `SendError::Full`; `Event::Writable` follows
when space frees. `send_bytes` does the same for `Framing::Bytes`. With
TLS, the stream seals from the segment into its record buffer.

### Connections over time
- `connect(addr, opts)`: timeout; optional `Reconnect { base, max, jitter }`
  keeps the handle across reconnects and reports `Event::Down` and
  `Event::Up`; queued sends are dropped on `Down`. Jitter comes from a
  seeded generator, so simulation stays deterministic.
- `listen(addr, opts)`: `reuse_port`, backlog, bind retry until a deadline
  on `EADDRINUSE`, a connection limit. Past the limit one refusal slot per
  listener accepts the connection, reports `Event::Refused { conn }` so the
  caller can send one frame, and closes after it. Accept failures for lack
  of descriptors back off from 5 ms, doubling to 1 s.
- `Event::Closed { conn, why }` distinguishes `Orderly` (peer finished after
  all data arrived), `Reset`, `Timeout`, `Protocol`, `Local`.
- `shutdown_write(conn)` sends what is queued, then shuts down the write
  side; `close(conn)` ends at once; `drain(deadline)` stops accepting, lets
  queued sends finish, and reports `Event::Drained`.

### Time
The caller passes `now` (nanoseconds from `Io::now_ns`) to `poll`.
Deadlines (connect, handshake, idle, reconnect, bind retry, accept backoff,
drain) sit in a fixed table; `next_deadline()` returns the earliest, so the
caller sleeps exactly as long as it may (smoltcp's `poll_at`). `poll`
handles at most a configured number of completions per call, so one turn of
the caller's loop stays bounded.

### TLS
`connect` and `listen` take an optional `ClientSetup` or `ServerSetup`; the
connection runs through `longshore_stream::Stream`, so any TLS provider
works. For a kernel-bypass transport, the record layer (`longshore_tls::
record`, which does no I/O) is reused directly.

### Additions to `Io`
- `Op::Shutdown { sock, how: Read | Write | Both }` (`IORING_OP_SHUTDOWN`,
  Linux 5.11; `shutdown(2)` in the portable backend; modelled in `SimIo`).
- The peer address of an accepted socket, readable by slot.
- Keepalive idle, interval and count applied with `SO_KEEPALIVE`.
- `listen` options: `SO_REUSEPORT` and the backlog.
All within Linux 6.1; options are set where the existing `no_delay` and
`keep_alive` are.

## Alternatives considered
- The `Io` trait as the seam: it is shaped by io_uring (operations, buffer
  ids), and a user-space TCP stack has no such operations.
- A cross-thread handle with its own I/O threads (like the S3 client):
  UltraGraphDB drives I/O from its own per-core loop; a runner can be added
  on top later without changing `Net`.
- A shared listening socket across cores: `SO_REUSEPORT` gives each core its
  own queue and needs no descriptor shared between rings.
- One mode of receiving: the ring wastes nothing on many small connections
  but copies frames that span buffers; direct receives copy nothing but post
  two receives per frame. Both are kept and measured.

## Risks / Trade-offs
- Frames spanning ring buffers are copied → bounded to those frames; links
  with large frames use `Rx::Direct`; change 2 measures both.
- Many connections with deep send queues exhaust send segments → `Full`
  with `Writable`, never an allocation; sizes are in `Config`.
- `Op::Shutdown` breaks outside `Io` implementations → noted as breaking;
  the in-tree backends are updated together.

## Migration Plan
New crate; UltraGraphDB adopts it in its own repository.

## Open Questions
- None for this change. Defaults (ring buffer size, receive mode, zero-copy
  threshold, segment size) are settled by change 2's measurements.
