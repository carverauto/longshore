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
caller's loop: owns the backend, decides where the thread waits
  Net<T: Transport>      handles, framing, held frames, send queues,
                         deadlines, reconnect, accept policy, drain
  Transport              backend seam: byte streams over lent buffers,
                         with an associated Driver the caller owns
    KernelTransport<IO: Io>    Driver = IO: io_uring | portable | simulator
    (change 3) XdpTransport    Driver = the core's AF_XDP sockets
```
`Net` holds everything that does not depend on how bytes reach the wire, so
a second transport reuses it whole. `Transport` speaks in slots, lent
receive buffers, reserved send space and transport events; it names no
`Op`, `BufId` or `Sock`.

### Net borrows the backend; it never owns it and never waits
Every existing component borrows the `Io` (`Client::on_completion(io, c)`),
and so does `Net`: `net.on_completion(&mut io, completion, &mut events)`
for a completion whose token is in `Net`'s range, `net.poll(&mut io, now,
&mut events)` for deadlines and queued sends, `net.next_deadline()`. `Net`
never calls `tick` and never blocks. The owner ticks once per turn, routes
completions by token (`Config::token_base`, as the HTTP client does) and
chooses where the thread sleeps.

This is what a database needs. A request is receive, log append, sync,
reply; network and file I/O for a partition belong on the thread that owns
it, and by default on one ring: one place to block, one system call per
turn for sends, writes and syncs together. The owner may instead run a
second ring on the same thread (a polled NVMe ring, `IORING_SETUP_IOPOLL`,
cannot carry sockets) or busy-poll a NIC queue (change 3) and check the
file ring without blocking. `Net` works unchanged in each case because it
only borrows.

### One `Net` per thread
No locks and no `Send` bound on the data path. A program with N cores runs
N `Net`s; messages between them use `longshore_mailbox`. Each core listens
on the same address with `SO_REUSEPORT`, so the kernel spreads connections
across cores without a shared socket (replacing UltraGraphDB's dup).

### Memory comes from the backend's pool, sized at build
The `Io` backend owns the one buffer pool, sized by `Limits`: a large
class, a small class and one buffer ring. `Net` allocates no buffer; its
`Config` names what it uses in the shared `Io`: a range of socket slots
(with the accept range inside it), a token base, a range of block-aligned
buffers for frames (see extents below), a range of buffers for send
segments (small or large class), a range for TLS record buffers when TLS
is used, and the shared buffer ring.
`Config::limits(base)` returns `Limits` with `Net`'s needs added (buffers,
sockets, and operations: each connection in ring mode holds one operation
for its armed receive), so an owner that also runs files or the S3 client
on the same `Io` sizes it once. `Net`'s own tables (connections, queues,
deadlines) are allocated in `Net::new`; nothing allocates after, checked
under `alloc_guard`. The TLS handshake keeps its sanctioned exception.

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

### Frame memory is extents: received where it will rest
Network buffers and log buffers are the same buffers. Frame memory is the
`Io` pool's block-aligned class (sockets may receive into and send from
it; only file operations require it), so a frame received in direct mode
lies contiguous, header first, in a buffer a direct-I/O log write takes as
it is, padded to the block size. An extent table in `longshore_common`
(`Extents`) keeps, per aligned buffer, a hold count and a generation, both
plain integers since one thread owns them: `Net` takes one hold when it
delivers a frame, `release` drops it, and the caller adds holds of its own
(the log until the write is durable, each peer's send queue until its send
completes). The buffer is free at zero; a `FrameId` or span with an old
generation is refused. Every payload byte is therefore moved once, by the
kernel's receive copy, and never again: not into the log, not into send
queues.

### Receiving: two modes, chosen per endpoint
- `Rx::Ring` (many connections, small frames): one armed receive per
  connection from the `Io`'s buffer ring. A frame that lies whole inside
  one ring buffer is lent in place. A frame that spans buffers, or is
  larger than one, is assembled into a frame buffer: one copy, for that
  frame only.
- `Rx::Direct` (links with large frames): the connection receives the
  header into its own small area, then the body straight into a frame
  buffer taken from the pool when the header arrives. No copy for any
  frame; two receives per frame. If the pool is empty the connection does
  not read until a frame is released.
Change 2's benchmarks set the defaults and the ring buffer size.

### Holding frames
`Event::Frame { conn, frame }` carries a `FrameId`; `net.frame(id)` gives
the bytes and `net.release(id)` returns the memory.
- A frame in a frame buffer (direct mode, or assembled) may be held as long
  as the caller likes: UltraGraphDB parks requests without copying them.
- A frame lent from a ring buffer pins that whole buffer, and the ring is
  shared by every connection on the `Io`: an empty ring ends every armed
  receive with `-ENOBUFS`. So a ring-lent frame is valid until the caller's
  next `poll`; `net.hold(id)` moves it into an extent (one copy; several
  small frames pack into one batch extent) to keep it longer, and frames neither released nor held by then are released.
  The ring therefore refills every turn whatever one consumer does.
Each connection has a held-bytes budget counting the frame buffers it
holds; at the budget the connection stops reading (its armed receive is
cancelled, or no receive is posted) and resumes when releases bring it
under. When the ring runs dry `Net` re-arms receives as buffers return.
Nothing is refused.

### Sending
A connection's send queue is a queue of spans `(buffer, offset, length)`.
`net.send_lent(conn, frame, range)` queues a span on an extent the caller
already holds, taking one more hold until the send completes: forwarding a
received frame, or replicating it to N peers, copies nothing. For frames
the caller builds, `net.send_frame(conn, len, |buf| ..)` reserves `len` bytes in the
connection's send queue, which is a chain of send segments taken from a
shared pool of registered buffers, and lets the caller serialise into them
in place. Frames queued back to back in a segment go out as one send;
sends at or above a size go zero-copy (`SEND_ZC`) on TCP sockets; Unix
sockets and TLS connections never do (the kernel would copy anyway, and
TLS sends its own record buffer). One send is in flight per connection, and
its segment belongs to the kernel until the send completes (for zero copy,
until the kernel's release notice), so a connection that keeps queueing
uses a second segment meanwhile: the segment range is sized at twice the
number of connections sending at once, at least. A full queue or an empty
segment pool returns `SendError::Full`; `Event::Writable` follows when
space frees. `send_bytes` does the same for `Framing::Bytes`. With TLS,
the stream seals from the segment into its record buffer.

### The turn
The owner's loop has a fixed order, offered as a small helper (`Turn`) and
documented for owners that write their own:
1. collect: one `tick` submits everything queued and takes completions;
2. storage completions first, so what became durable can be answered;
3. network completions: sends, then receives, frames to the state machines;
4. flush: the turn's log extents in one linked write and sync
   (`submit_chain`), and `net.poll` for queued sends;
5. sleep until `next_deadline`, or go again if work is queued.
The turn is the group commit: frames received in turn N are written in turn
N's one submission, together with the replies for turn N-1. Completions
are dispatched by token class out of the batch already collected, so a
sync completion never waits behind a burst of receives, without a second
ring. The batch is bounded by the `Completions` capacity and the buffer
ring's size, which bounds the turn.

### Connections over time
- `connect(addr, opts)`: timeout; optional `Reconnect { base, max, jitter }`
  keeps the handle across reconnects and reports `Event::Down` and
  `Event::Up`; queued sends are dropped on `Down`. Jitter comes from a
  seeded generator, so simulation stays deterministic.
- `listen(addr, opts)`: `reuse_port`, backlog, bind retry until a deadline
  on `EADDRINUSE`, a connection limit, and `peer_addr`. An armed accept
  installs registered descriptors and returns no address, and on Linux 6.1
  nothing can ask a registered descriptor for its peer; so a listener with
  `peer_addr` accepts one connection per operation with an address buffer
  and re-arms itself, and `net.peer(conn)` returns the address. Without it
  the listener uses the armed accept. Past the limit one refusal slot per
  listener accepts the connection, reports `Event::Refused { conn }` so the
  caller can send one frame, and closes after it. Accept failures for lack
  of descriptors back off from 5 ms, doubling to 1 s.
- `Event::Closed { conn, why }` distinguishes `Orderly` (peer finished after
  all data arrived), `Reset`, `Timeout`, `Protocol`, `Local`.
- `shutdown_write(conn)` sends what is queued, then shuts down the write
  side; `close(conn)` ends at once; `drain(deadline)` stops accepting, lets
  queued sends finish, and reports `Event::Drained`.

### Time
The caller passes `now` (nanoseconds from the backend's clock) to `poll`.
Deadlines (connect, handshake, idle, reconnect, bind retry, accept backoff,
drain) sit in a fixed table; `next_deadline()` returns the earliest, so the
caller sleeps exactly as long as it may (smoltcp's `poll_at`), by a timeout
operation or a bounded wait of its own. The owner routes completions, so it
bounds how many it hands to `Net` per turn; `poll` itself sends at most a
configured number of queued segments per call.

### TLS
`connect` and `listen` take an optional `ClientSetup` or `ServerSetup`; the
connection runs through `longshore_stream::Stream`, so any TLS provider
works. `Stream` today receives only with `Op::Recv` into a buffer it is
given, which serves direct mode. For ring mode it gains a second receive
path: `Stream::on_ring_recv(io, ring_buf, len, dst)` opens the records in a
ring buffer straight into the destination (frame memory), keeps the tail of
a record that continues in the next ring buffer in the connection's TLS
receive buffer, and returns the ring buffer at once, since plaintext never
stays in it. The HTTP server can use the same path later. For a
kernel-bypass transport, the record layer (`longshore_tls::record`, which
does no I/O) is reused directly.

### Additions to `Io`
- `Op::Shutdown { sock, how: Read | Write | Both }` (`IORING_OP_SHUTDOWN`,
  Linux 5.11; `shutdown(2)` in the portable backend; modelled in `SimIo`).
- `Op::Accept` reports the peer's address (kept per operation slot, read
  with `Io::peer_addr(sock)` after the completion).
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
- `Net` owning the `Io` and ticking it: a partition could then not run its
  log, the S3 client and its connections on one ring.
- Network and file I/O on separate threads: two hand-offs and wake-ups per
  request, for nothing a second ring on the same thread does not give.
- Holding ring-lent frames without a copy: one slow consumer would pin the
  shared ring and stall every connection.
- One mode of receiving: the ring wastes nothing on many small connections
  but copies frames that span buffers; direct receives copy nothing but post
  two receives per frame. Both are kept and measured.

## Risks / Trade-offs
- A log written in place from extents needs padding to the block size
  between extents → the caller's log format must allow it (UltraGraphDB's
  decision); small frames are packed into one batch extent instead.
- Frames spanning ring buffers, and ring-lent frames the caller holds, are
  copied → bounded to those frames; links with large or parked frames use
  `Rx::Direct`; change 2 measures both.
- Direct mode with many connections and 1 MiB frames could ask for
  gigabytes of locked memory → the frame range is sized by frames in
  flight, not by connections; a connection waits for a frame buffer.
- A listener that reports peer addresses accepts one connection per
  operation → one more submission per accept; measured in change 2.
- Many connections with deep send queues exhaust send segments → `Full`
  with `Writable`, never an allocation; sizes are in `Config`.
- `Op::Shutdown` breaks outside `Io` implementations → noted as breaking;
  the in-tree backends are updated together.

## Migration Plan
New crate; UltraGraphDB adopts it in its own repository.

## Open Questions
- None for this change. Defaults (ring buffer size, receive mode, zero-copy
  threshold, segment size) are settled by change 2's measurements.
