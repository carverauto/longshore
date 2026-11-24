# TCP connections

`longshore::tcp` (the crate `longshore_tcp`) is for protocols that run
straight over TCP: replication, messaging, a database's own wire protocol.
It gives a fixed set of connections and listeners, whole frames, and frame
memory that a log and other connections can use without copying. This page
describes the API, the two ways of receiving, TLS, and how to size it.

## What it is

`Net<T>` is the set. It is driven from the caller's own loop on the
caller's own thread: it borrows the I/O backend on each call, never waits
and never collects completions itself. File operations, an object storage
client and several sets can therefore share one backend and one thread.

`T` is the transport. `KernelTransport<IO>` carries the set over the
kernel's TCP and Unix-domain stream sockets through the `Io` trait, so the
same program runs on `UringIo`, on `PosixIo` (`kqueue`, `epoll`) and in the
simulator. A kernel-bypass transport is planned behind the same API.

Everything is allocated when the set is built. After that nothing
allocates, except rustls during a TLS handshake.

## One turn

```text
io.tick(wait, &mut completions)            collect
net.on_completion(&mut io, c)              route: false means "not mine"
net.next_event()                           pull events until None
net.next_frame(&mut io, conn)              pull frames until None, after Readable
net.poll(&mut io, io.now_ns())             deadlines, queued sends, retries
net.next_deadline()                        the latest time to call poll again
```

Completions only mark connections. The caller pulls events and frames, so
no event buffer can overflow, and not pulling is the backpressure: a
connection whose frames are not taken stops reading.

Events and frames can be pulled in any order. In particular the end of a
connection is never reported while frames received on it are still to be
pulled: `Readable` comes first, and `Closed` (or `Finished`, or `Down`)
only after the last frame has been taken.

`Turn` (in `longshore::io::turn`) hands a batch of completions out by token
range, storage first, then network. With a log on the same backend that
makes the turn the group commit: syncs are seen before the frames that
arrived behind them.

The caller owns time. `poll` is told the current time, and `next_deadline`
says when the earliest connect, handshake, idle, reconnect, bind or accept
deadline is due.

## Connections and listeners

- `connect(io, addr, opts)` returns a `Conn` at once; `Event::Up` follows,
  or `Event::Closed`. `addr` is a `SocketAddr` or `Addr::unix(path)`.
- `ConnOpts` sets the framing, the receive mode, the connect, handshake and
  idle timeouts, how much frame memory the caller may hold before the
  connection stops reading, and reconnecting. A reconnecting connection
  keeps its handle: `Event::Down`, then `Event::Up` again, with delays that
  double to a maximum, with jitter.
- `listen(io, addr, ListenConfig)` returns a `Listener`. A listener can
  share its port across threads (`with_reuse_port`), retry a bind refused
  as in use (`with_bind_retry_ns`), report peer addresses
  (`with_peer_addr`, at the cost of one accept operation per connection),
  and hold a limit (`with_limit`): one connection over the limit is
  accepted as `Event::Refused`, may be sent one frame (queued in the turn
  the event is pulled), and is closed without a further event.
- `close_listener` stops a listener. Its slot can hold a new listener
  once its close and its last accept have ended, a turn or two later;
  until then `listen` may answer `NoSlots`. A connection it accepted is
  announced with its handle even when the listener has been closed since.
- A handle carries a generation. Once its connection has ended it is stale
  and reaches nothing, whoever has the slot next.
- `Event::Closed { conn, why }` says how a connection ended: `Orderly`
  (the peer's stream ended between frames, and nothing failed), `Reset`
  (a broken connection, or a stream that ended inside a frame), `Timeout`,
  `Protocol` (a frame the framing does not allow) or `Tls`. It comes when
  everything received has been pulled. A connection whose stream has ended
  and whose frames are never pulled waits for its caller like any other;
  the idle timeout, if one is set, or `close` ends the wait.
- By default the end of the peer's stream ends the connection: what is
  still queued for it is dropped. With `ConnOpts::with_half_close(true)`
  a peer that finishes in order (sends its request, shuts its write side
  down, and waits) can still be answered: `Event::Finished(conn)` says the
  peer has finished, sends go on, and the connection ends as `Orderly`
  after `shutdown_write`, or with `close`. A listener sets it for the
  connections it accepts.
- `shutdown_write` finishes after what is queued, and receiving goes on;
  `close` ends at once, with no event; `drain` stops accepting and reports
  `Event::Drained` when queued sends have gone or its deadline passes.
- `is_up` says a connection has both directions open; `can_send` says a
  send would be accepted, which is also true of a half-closed connection.
- A call on a connection that is not open for it fails with `NotUp`: a
  send or a shutdown before it is up or after its end, and a send after
  `shutdown_write`. A second `shutdown_write` does nothing. `Stale` is
  for a handle that names no connection any more.

## Framing

`Framing::Header(HeaderFraming)` describes a fixed-size header that carries
the frame's length: where the field is, its width (1, 2, 4 or 8 bytes), its
byte order, whether it counts the header, and the longest frame allowed.
`HeaderFraming::length_prefix(max)` is the common four-byte big-endian
prefix. A frame is delivered whole, header included. `Framing::Bytes`
delivers bytes as they arrive.

## Receiving

A received frame is lent where it lies. There are two modes, per
connection:

- `Rx::Ring`: one armed receive delivers into a ring of buffers shared by
  all connections. A frame that lies whole in one ring buffer is lent
  there, and is good until the next `poll`; `hold` moves it to frame memory
  to keep it. A frame that spans ring buffers is put together in frame
  memory. Input that is not pulled is moved out of the ring at the next
  `poll`, so one slow consumer cannot keep the ring from the others. This
  mode costs the fewest operations for many connections with small frames.
- `Rx::Direct`: the header is received first, then the body straight into
  the frame's own memory, at the start of a block-aligned buffer. Nothing
  is copied, and the frame can go to a direct-I/O write as it is. This is
  the mode for large frames and for frames that are logged.

## Frame memory and holds

Frame memory is a range of block-aligned buffers (`Extents`) with a hold
count each. A frame in frame memory has the caller's hold; `send_lent`
adds one until its send is done; a file write adds one with
`net.extents().hold(..)` and releases it when the sync has completed. The
buffer is free again when the last hold is gone, and a stale reference to
it is refused.

So one received frame can be written to a log with direct I/O and sent to
several peers from the memory it was received into. Its bytes are moved
once, by the receive.

## Sending

`send_frame(io, conn, len, fill)` writes a frame straight into a send
segment; frames queued in one turn share segments and leave in fewer
sends. `send_bytes` copies a slice in. `send_lent(conn, &frame)` queues a
held frame from where it lies. A full queue returns `SendError::Full`, and
`Event::Writable` follows when there is room. So does an empty segment
pool: each segment that returns is announced to the connection that has
waited for one longest. Sends above
`Sizes::with_zero_copy_min` are zero-copy on TCP without TLS.

## TLS

`Sizes::with_tls(true)` reserves two buffers of the segment size per
connection, and `KernelTransport::with_tls(Tls)` takes a `ClientSetup`, a
`ServerSetup` or both. `connect_tls(io, addr, server_name, opts)` opens a
protected connection; `ListenConfig::with_tls(true)` protects what a
listener accepts. Any compiled-in provider (`aws-lc`, `ring`, `rustls`)
works; the setup chooses.

- `Event::Up` and `Event::Accepted` come when the handshake is done. The
  connect timeout covers a client's handshake; an accepted connection that
  has not finished after `with_handshake_timeout_ns` is closed unannounced.
- In ring mode records are opened from the ring buffer straight into frame
  memory. In direct mode a record that does not fit what is left of the
  frame is opened in the connection's own buffer and copied, so ring mode
  is the faster one with TLS.
- `shutdown_write` sends a close alert. An end of stream without one is
  reported as `Reset`.

## Sizing

`Config::plan(base, sizes)` adds what a set needs to the `Limits` of a
backend and returns both; build the backend from the `Limits` and the set
from the `Config`. Plan several components one on top of the other.

| `Sizes` | Meaning | Default |
|---|---|---|
| `new(connects, accepts, listeners)` | Connections the caller opens, connections accepted, listeners | none |
| `with_frames(count, size)` | Frame memory; `size` is a multiple of the block size and at least the longest frame | 64 of 1 MiB |
| `with_segments(count, size)` | Send segments; a frame sent with `send_frame` fits one | 64 of 64 KiB |
| `with_ring(count, size)` | The shared receive ring | 64 of 64 KiB |
| `with_header_max(bytes)` | The longest header | 256 |
| `with_spans(n)` | Queued sends per connection | 16 |
| `with_zero_copy_min(bytes)` | Sends at least this long are zero-copy | 16 KiB |
| `with_tls(on)` | Room for TLS; segments are then at least `TLS_SEGMENT_MIN` (36 KiB) | off |
| `with_token_base(base)` | The first of the set's tokens; it uses `Config::tokens()` of them | 0 |

Size `accepts` above every listener's `limit`. With all accept slots in
use the kernel has nowhere to put the next connection it accepts and drops
it, and the set waits (5 ms, doubling to 1 s) before accepting again. The
limit refuses in order.

With TLS in ring mode, frame memory holds at least one record
(16,401 bytes).

## How it is checked

`longshore::tcp::check` ships in the library, as the `Io` contract checks
do. `check::run_all` exchanges frames of every kind in both receive modes
over TCP and Unix-domain sockets and returns what arrived, which must be
the same on every backend and with TLS; `check::storage` runs a log and
three replicas on one backend. Both run on the simulator (also with
partial receives and short sends) and on `PosixIo` in the crate's tests,
and on `UringIo` and `epoll` on a real kernel from `tcp_uring`, under the
allocation guard and once per TLS provider. See
[Building, testing and running against a store](testing.md).

## Not there yet

- Throughput and latency figures: the benchmarks are the next change.
- A simulated packet network (loss, reordering) and the AF_XDP transport.
- Stopping an armed accept when the accept range is full.
