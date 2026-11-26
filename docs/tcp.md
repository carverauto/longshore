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
  `Event::Drained` when queued sends have gone and the memory of every
  zero-copy send is back, or its deadline passes.
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

A log that wants a frame's body, not its header, at a block boundary asks
for it in the framing: `HeaderFraming::with_body_at_block(4096)`. In
direct mode the frame then starts `lead()` bytes into its memory (3,840
for a 256-byte header), so that the body starts at a block and goes to a
direct-I/O write from where it lies; `Frame::span` says where. Name the
block of the file the body goes to. Frame memory must be whole blocks of
it and hold the lead and the largest frame, and ring mode refuses the
option: there a frame has no memory of its own. Under TLS the body lies
at the block as well, but a record that carries the header with the body
is opened in the connection's own memory and moved from there, as in
direct mode without the option. Measured between two machines with
`tcp_uring path`, it saves the move a log would make: about 5 % of the
log node's CPU a block, three runs each, anywhere from 3 to 9 % by run
(`bench/results/m31-tcp-zero-copy-receive-spike-dgx-spark.md`).

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
waited for one longest. A connection that sends again without waiting,
and is not refused, has seen the room for itself and is not told. Sends
above
`Sizes::with_zero_copy_min` are zero-copy on TCP without TLS.

A zero-copy send is reported as soon as the kernel has taken its bytes,
and the connection's next send follows at once. The kernel goes on
reading the memory until the peer has acknowledged the bytes; until then
the memory is pinned: a send segment stays out of the pool though its
place in the queue is free, and frame memory keeps one more hold. A
connection has so many such sends out at most, and the set so many in
all (`Sizes::with_zero_copy_sends`, 4 and 256), and a connection sends
with a copy while either is reached. Each is an operation of the
backend while it is out, so the second number is what the set adds to
`Limits::ops_max` for them. Its slot is not free, and a drain not
done, before their memory is back. With `with_zero_copy_sends(0, 0)` a
connection waits for the memory of each zero-copy send before the next,
which over a network is one send a round trip: measured between two
machines, 64 KiB sends then carried about 1 GB/s where copied ones
carried 6, and 5.6 to 6.1 with the default
(`bench/results/m32-tcp-zero-copy-send-dgx-spark-pair.md`).

A connection with `ConnOpts::with_frame_on_packet` sends every frame as
a send of its own: a frame is not put into a segment behind another, and
the next frame's send begins when the transport has taken the last
frame whole, which for a zero-copy send is its first report, not the
return of its memory. It is for a peer that receives into pages
(`Rx::ZeroCopy`), which can lend a frame where it lies only when the
frame starts a packet. A frame then takes a segment however short it is,
so segments are counted in frames. What the option cannot give is the
packet itself. TCP still does these to it:

- it puts the first bytes of a send into an unsent packet that has room,
  so a frame sent while bytes of the one before wait in the socket may
  not start a packet;
- with Nagle's algorithm it holds a short last packet back to do just
  that: switch it off on the backend (`Io::no_delay(true)`);
- it joins packets that it sends again, and cuts a packet short when the
  peer's window or its own congestion state says so.

A frame that did not start a packet is moved by its receiver once, as in
direct mode; nothing else follows from it.

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
| `with_ring(count, size)` | The shared receive ring | 64 of 16 KiB |
| `with_header_max(bytes)` | The longest header | 256 |
| `with_spans(n)` | Queued sends per connection | 16 |
| `with_zero_copy_min(bytes)` | Sends at least this long are zero-copy | 16 KiB |
| `with_zero_copy_sends(per_connection, in_all)` | Zero-copy sends whose memory the kernel still reads, a connection and in the whole set; each is an operation of the backend until it is back | 1 and 256 |
| `with_tls(on)` | Room for TLS; segments are then at least `TLS_SEGMENT_MIN` (36 KiB) | off |
| `with_token_base(base)` | The first of the set's tokens; it uses `Config::tokens()` of them | 0 |

Size `accepts` above every listener's `limit`. With all accept slots in
use the kernel has nowhere to put the next connection it accepts and drops
it, and the set waits (5 ms, doubling to 1 s) before accepting again. The
limit refuses in order.

With TLS in ring mode, frame memory holds at least one record
(16,401 bytes), and there are at least two frame buffers: a frame that
spans records needs a second while it is put together.

## States and invariants

A connection is in one of ten states, and every call and every report of
the transport does a set thing in each. The full tables, with the listener's
and the TLS phases of the kernel transport, are in
`openspec/changes/archive/2026-11-25-add-tcp-simulation-and-benchmarks/design.md`;
the model test holds the code to them cell by cell.

| State | What it is |
|---|---|
| `Free` | No connection in the slot. |
| `Connecting` | The caller's connect is on its way; with TLS, its handshake too. |
| `Securing` | Accepted, and its TLS handshake is running. It has no handle yet. |
| `Up` | Open both ways. |
| `Refused` | Accepted over its listener's limit: it is told so in one frame and closed. |
| `Finishing` | Its stream has ended, and input received before the end is still to be pulled. |
| `HalfClosed` | The peer has finished in order; the caller may still send (with `with_half_close`). |
| `Dying` | It has ended, and the caller has not pulled the event that says so. |
| `Closing` | Its close is with the transport, or operations it ended have yet to report. |
| `Backoff` | A reconnecting connection waits to dial again. |

What holds in every state, whatever the order of calls and reports:

Checked after every step of the model test, and by debug assertions where
that is cheap.

- **I1 Slot ownership.** A connection slot is owned by at most one
  connection; it is freed only when its close has completed and nothing is
  in flight for it.
- **I2 One in flight.** Per connection at most one send not yet
  reported, one receive (or one armed receive), and one of connect,
  handshake, shutdown and close. Per listener at most one accept and one
  close. Zero-copy sends that were reported and whose memory the
  transport still reads are in flight too, up to
  `Sizes::with_zero_copy_sends` a connection and in all, each told apart
  by a number of its own.
- **I3 Quiet when free.** Nothing is in flight, queued or held for a slot
  that is `Free` or `Backoff`.
- **I4 Handles.** A retired handle reaches nothing. A handle the caller
  still holds never comes to name another connection or listener.
- **I5 Events.** No event for a retired handle. `Closed` is the last event
  of a connection. `Up` and `Down` alternate. `Accepted` or `Refused`
  comes at most once. `Finished` comes at most once per time the
  connection is up, only with half-close on, and before `Closed` or
  `Down`.
- **I6 Delivery.** Bytes are delivered exactly once, in order. No end
  event (`Finished`, `Closed`, `Down`) is owed while anything received is
  left to pull, in whatever order the caller pulls events and frames and
  whatever the pools hold at that moment. Only `close()` and the idle
  timeout drop input.
- **I7 The end.** `Orderly` only if the stream ended between frames (with
  TLS: after a close alert) and no send failed. Otherwise `Reset`, `Tls`,
  `Protocol` or `Timeout`. `Finished` only for an end that is in order.
- **I7a Half-close.** After `Finished` the connection can be sent on
  until the caller shuts it down or closes it; with the option off the
  peer's end is the connection's end.
- **I8 Memory.** Every extent, segment and ring buffer has exactly one of:
  free, or held with a count that equals its holders. A zero-copy send
  whose memory the transport still reads is a holder. Counts never go
  below zero or wrap. With no connection open and no frame held, every
  pool is full.
- **I8a Bytes in flight.** No byte is written while a send still reads
  it: a segment a zero-copy send reads is in no queue that can grow into
  the bytes it sent and is not in the pool, and frame memory it reads is
  held.
- **I9 Deadlines.** A slot has a deadline only in a state whose row has
  one, and `next_deadline` is the earliest of them.
- **I10 Wakes.** Whoever was refused for lack of room (`Full`, a paused
  read, an accept that waits) is told, or resumed, once there is room,
  without any further call of the caller's being needed.
- **I11 Progress.** If the caller pulls every event and frame and releases
  what it holds, and the peer is not silent, the run does not stall.
- **I12 Allocation.** Nothing allocates after build, the TLS handshake
  aside.

## Defaults and what they are from

The defaults of `Sizes` and `ConnOpts` are provisional: they are from runs
over loopback in a Linux container
(`bench/results/m21-tcp-loopback-container-aarch64.md`), which can say which
setting is better than which on that machine and nothing of a network card.
Runs on hosts with 200 and 300 Gb/s cards are to settle them.

- **Receive mode: ring.** Ahead for frames smaller than a ring buffer.
  Choose `Rx::Direct` for frames of many ring buffers: it was level from
  4 KiB and well ahead at 1 MiB.
- **Ring buffers: 16 KiB.** Carried most in bulk and cost small requests
  little.
- **Send segments: 64 KiB.** Larger ones gained nothing.
- **Zero-copy from 16 KiB.** Unchanged: loopback cannot tell.
- **Four zero-copy sends a connection with their memory out.** Measured
  between two machines over a 200 Gb/s card
  (`bench/results/m32-tcp-zero-copy-send-dgx-spark-pair.md`): four
  carried as much as any number tried and about halved the sending
  thread's CPU against one, with which most sends are copied; eight was
  slower in two cases of nine. Nothing is lent to a loopback address
  or a Unix-domain socket: the kernel copies there whatever it is
  asked, and zero-copy sends over loopback carried less than copied
  ones (eight out: 7.5 against 11 GB/s).
- **How a log is made durable, and whether a thread waits or spins:** no
  default. On the container's virtual disk the two ways of syncing could
  not be told apart. A single connection was answered in 24 µs with both
  threads waiting, in 8 µs with the one under test spinning, and in 3 µs
  in one short run with both spinning, a core each.

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

- Figures from a network card: `tcp_uring rr`, `bulk` and `path` measure,
  and what there is so far is loopback in a container.
- The AF_XDP transport. The simulated packet link it is to be tested on
  is there (`longshore::sim::PacketLink`).
- Stopping an armed accept when the accept range is full.
