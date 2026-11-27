# tcp-connections Specification

## Purpose
Connections for protocols that run straight over TCP or Unix-domain
sockets (`longshore_tcp`): a fixed set of connections and listeners driven
from the caller's loop on a backend it shares with file I/O, with framing,
frames lent in place, counted frame memory, optional TLS, and time owned by
the caller. Described in `docs/tcp.md`.
## Requirements
### Requirement: Connection set allocated at build
`longshore_tcp` SHALL allocate its connection, listener, queue and deadline
tables when the set is built, SHALL take its frame, send and TLS buffers
and its socket slots from ranges of the backend's pool named in its
configuration, and SHALL NOT allocate afterwards, the TLS handshake
excepted.

#### Scenario: Traffic under the allocation guard
- **WHEN** connections are opened, frames are sent and received both ways, and connections are closed and reopened under `alloc_guard` after the set is built
- **THEN** no allocation is reported

#### Scenario: Out of connection slots
- **WHEN** a connect is asked for with every connection slot in use
- **THEN** it fails with an error saying so, and nothing is allocated

### Requirement: Handles that cannot reach a reused slot
Connections and listeners SHALL be named by handles that carry a
generation, and an operation with a handle whose slot has since been freed
SHALL fail without touching the slot's current connection.

#### Scenario: Stale handle
- **WHEN** a frame is sent with the handle of a connection that has closed and whose slot holds a new connection
- **THEN** the send fails as stale and the new connection is unaffected

### Requirement: Framing
The set SHALL deliver either raw bytes, which the caller consumes in place,
or whole frames described by a fixed-size header holding the frame's length
at a configured offset, width and byte order, counting the header or not,
up to a maximum frame size. A length outside the bounds SHALL close the
connection as a protocol error.

#### Scenario: Four-byte length prefix
- **WHEN** a peer sends frames with a big-endian four-byte length prefix, split across many reads
- **THEN** each frame is delivered whole and once, in order

#### Scenario: Header carrying the frame size
- **WHEN** a peer sends frames with a 256-byte header whose little-endian u32 at offset 40 is the whole frame's length
- **THEN** each frame is delivered whole, header included

#### Scenario: Frame too large
- **WHEN** a header announces a frame larger than the maximum
- **THEN** the connection is closed as a protocol error and no frame is delivered

### Requirement: Received frames lent in place
Received frames SHALL be lent to the caller without a copy whenever the
frame lies within one receive buffer or the connection receives directly
into frame memory. A frame in frame memory SHALL stay valid until the
caller releases it; a frame lent from the shared buffer ring SHALL stay
valid until the caller's next `poll` unless the caller holds it, which
moves it into frame memory. A connection SHALL stop reading while the
frame memory it holds exceeds its held-bytes budget.

#### Scenario: Frames held across turns
- **WHEN** the caller holds received frames over several calls to `poll` and then releases them
- **THEN** their bytes stay unchanged until released, and their memory is reused after

#### Scenario: A slow consumer does not starve the ring
- **WHEN** one connection's frames from the buffer ring are neither released nor held while other connections keep receiving
- **THEN** every ring buffer is back in the ring after the next `poll`, and the other connections keep receiving

#### Scenario: Backpressure by not reading
- **WHEN** the caller holds frames up to a connection's budget while the peer keeps sending
- **THEN** the connection reads no further until frames are released, and then delivers the rest in order

#### Scenario: No copy in direct mode
- **WHEN** a connection in direct receive mode receives frames of up to the maximum frame size
- **THEN** each frame's bytes are received straight into the memory lent to the caller

### Requirement: One move per payload byte
Frame memory SHALL be block-aligned buffers with a hold count and a
generation each; a received frame SHALL be usable, where it was received,
as the source of a direct-I/O file write and of sends to any number of
connections, and its buffer SHALL be reused only when every hold is
released.

#### Scenario: Receive, log, replicate
- **WHEN** a frame received in direct mode is written to a file with direct I/O and sent to three other connections
- **THEN** the file and the three peers hold the frame's bytes, and the frame was not copied after it was received

#### Scenario: Buffer reused only at zero holds
- **WHEN** a frame is held by a file write and two sends, and they complete one after another
- **THEN** its buffer returns to the pool after the last of them and not before, and an old `FrameId` for it is refused

### Requirement: Sending from registered memory
The caller SHALL serialise each frame directly into registered send memory
reserved by the set; the set SHALL send frames queued back to back with as
few sends as the memory allows, use zero-copy sends at or above a
configured size, and answer a full send queue with an error followed by a
writable event when space frees.

#### Scenario: Many small frames
- **WHEN** the caller queues one hundred 512-byte frames on one connection before the next `poll`
- **THEN** they reach the peer in order using fewer sends than frames

#### Scenario: Queue full
- **WHEN** the caller queues frames until the send queue is full
- **THEN** the next send fails as full without allocating, and a writable event follows once the peer has taken enough data

### Requirement: Connecting and reconnecting
A connect SHALL fail after a configured timeout, and a connection opened
with a reconnect policy SHALL keep its handle, report when it goes down and
comes up, and redial with exponential backoff and jitter between a base and
a maximum delay.

#### Scenario: Peer restarts
- **WHEN** the peer of a reconnecting connection closes and is listening again after a while
- **THEN** the set reports the connection down, redials at growing intervals, and reports it up under the same handle

#### Scenario: Connect timeout
- **WHEN** a connect does not complete within its timeout
- **THEN** the connection is reported closed with a timeout

### Requirement: Listening
A listener SHALL support sharing its port across the program's cores with
`SO_REUSEPORT`, retrying a bind refused with `EADDRINUSE` until a deadline,
limiting its connections with one refusal slot through which the caller can
send a final frame, backing off accepts after a failure for lack of
descriptors, and reporting each accepted connection's peer address when
asked to.

#### Scenario: One port, every core
- **WHEN** four sets on four threads listen on the same address with port sharing and clients connect many times
- **THEN** every connection is accepted by exactly one set

#### Scenario: Peer address
- **WHEN** a listener opened with peer addresses accepts a connection from a known address and port
- **THEN** the set reports that address and port for the connection

#### Scenario: Over the limit
- **WHEN** a client connects while the listener is at its connection limit
- **THEN** the caller is told of a refused connection, may send it one frame, and the set closes it after that frame

### Requirement: Closing
The set SHALL report how each connection ended (orderly, reset, timeout,
protocol error, TLS failure, or closed locally), SHALL shut down the write side after
sending what is queued when asked, and SHALL drain on request: stop
accepting, finish queued sends, then report that it has drained.

#### Scenario: Orderly end versus reset
- **WHEN** one peer closes after sending all its data and another resets
- **THEN** the first is reported orderly after all its frames, the second as reset

#### Scenario: Drain
- **WHEN** the set is asked to drain with frames still queued
- **THEN** it accepts no new connection, sends the queued frames, and reports drained before the deadline

### Requirement: Time owned by the caller
The set SHALL take the current time from the caller on every `poll`, SHALL
report the earliest deadline it needs, and SHALL neither wait for nor
collect completions itself: the caller routes them to it.

#### Scenario: Sleeping until the next deadline
- **WHEN** the only pending work is a connect timeout
- **THEN** `next_deadline` returns that timeout's instant, and a `poll` at that instant reports the timeout

### Requirement: One backend shared with other I/O
The set SHALL borrow its backend on each call, so that file operations and
other clients run on the same backend and thread, each receiving only its
own completions.

#### Scenario: Log writes and frames on one ring
- **WHEN** one thread drives file writes and syncs and a TCP set on the same `Io`, routing completions by token
- **THEN** every file operation and every frame completes correctly, and neither receives the other's completions

### Requirement: Storage completions before network completions
The turn helper SHALL dispatch a batch of completions by class, storage
before network, and SHALL submit a turn's file writes, sync and sends
together.

#### Scenario: A sync behind a burst
- **WHEN** one batch holds a file sync completion after two hundred receive completions
- **THEN** the sync completion is dispatched first

### Requirement: Transports behind one API
The set's API SHALL name no type of a particular backend, and the kernel
transport SHALL run over the `Io` trait on the ring, the portable backend
and the simulator, for TCP and Unix-domain sockets, with or without TLS
from any TLS provider.

#### Scenario: Same program on three backends
- **WHEN** the same request-and-response exchange runs on `UringIo`, `PosixIo` and `SimIo`
- **THEN** it produces the same frames in the same order on each

#### Scenario: TLS per endpoint
- **WHEN** a listener is given a server setup and a client connects with a client setup, in ring mode and in direct mode
- **THEN** frames cross the connection encrypted and arrive unchanged

#### Scenario: Handshake that never starts
- **WHEN** a peer connects to a listener with TLS and sends nothing until the handshake timeout
- **THEN** the set closes the connection without announcing it, and its slot serves the next peer

#### Scenario: Certificate for another name
- **WHEN** a client connects with TLS to a server whose certificate does not carry the expected name
- **THEN** the connect ends with a TLS failure

#### Scenario: End without a close alert
- **WHEN** a protected connection's peer closes without sending a close alert
- **THEN** the connection is reported as reset, not as ended in order

### Requirement: Invariants under any order of operations
The TCP set SHALL keep its stated invariants (slot ownership, one operation
of a kind in flight, handles, event order, exactly-once in-order delivery,
the reported end, memory accounting, deadlines, wakes, progress, no
allocation) after every step of any sequence of calls, completions and
deadlines, including calls made in the wrong state or with stale handles
and completions delivered in any order.

#### Scenario: Random operations
- **WHEN** seeded random calls, faults and time steps run against a set with the smallest pools, with a tick's completions delivered in a seeded order
- **THEN** every invariant holds after every step, and what each connection delivered equals what its peer sent, in order

#### Scenario: Close racing with a completion
- **WHEN** a connection is closed while a connect, a handshake, a send, a receive or a shutdown is in flight, and the close's completion arrives before, between and after theirs
- **THEN** the slot is freed exactly once, after all of them have ended, and no event names the closed connection

#### Scenario: Slot taken again
- **WHEN** a slot is given to a new connection, by the set or by the kernel's accept, right after its previous connection was closed
- **THEN** nothing of the previous connection (a late completion, a queued event, held memory, a deadline) reaches the new one

#### Scenario: Failing seed
- **WHEN** a run fails
- **THEN** it reports its seed and step count, and replaying them fails in the same way

### Requirement: Exhaustion without loss
The TCP set SHALL refuse or pause without losing or duplicating data when
a pool it takes from is empty (connection slots, frame memory, hold
counts, send segments, a connection's send queue, the buffer ring, the
driver's operation slots), and SHALL resume or tell the caller when there
is room, with no further call needed to trigger it.

#### Scenario: Frame memory exhausted at the end of the stream
- **WHEN** a peer's last frame and the end of its stream arrive while every frame buffer is held, on a plain and on a TLS connection, in ring and in direct mode, with and without half-close
- **THEN** the frame is delivered once frame memory is released, and the end is reported after it as orderly

#### Scenario: Shared pool empty
- **WHEN** a send is refused because the shared segment pool is empty while the connection's own queue has room
- **THEN** the connection is told it can send once a segment is returned, also when the connection told before it has gone

#### Scenario: Every pool
- **WHEN** each pool in turn is the only one exhausted during an exchange
- **THEN** the exchange completes after the pool has room again, and every byte arrives once and in order

#### Scenario: More connections than frame buffers
- **WHEN** more connections than there are frame buffers each receive frames that do not fit a ring buffer, plain or over TLS, while the caller pulls and releases what it is given
- **THEN** every frame of every connection is delivered: no connection waits for ever for memory or ring buffers that other waiting connections hold

### Requirement: End of stream after everything received
The TCP set SHALL NOT report the end of a connection (`Finished`, `Closed`
or `Down`) while anything received on it is left to pull, in whatever
order the caller pulls events and frames, and SHALL report the end as
orderly only when the stream ended between frames, with TLS after a close
alert, and no send failed.

#### Scenario: Events drained before frames
- **WHEN** frames and the end of the stream arrive in one turn and the caller pulls every event before it pulls any frame
- **THEN** no end event is among them, every frame can still be pulled, and the end is reported after the last one

#### Scenario: Data and end in one turn
- **WHEN** frames, a partial frame and the end of the stream arrive in the same turn
- **THEN** the whole frames are delivered and the end is then reported as a reset

#### Scenario: Frame that is only a header
- **WHEN** a header announcing an empty body arrives in two pieces and the stream then ends
- **THEN** the frame is delivered and the end is reported as orderly

#### Scenario: End with frame memory exhausted
- **WHEN** the stream ends while every frame buffer is held, with input not yet pulled
- **THEN** no end is reported until frame memory is released and the input pulled, or the idle timeout passes, or the caller closes

#### Scenario: Closed under it
- **WHEN** the caller closes a connection whose stream has ended with input left
- **THEN** the input is dropped, no event follows, and all its memory is free once its operations have ended

### Requirement: Half-close by the peer
The TCP set SHALL, for a connection opened or accepted with half-close,
keep it open for sending after its peer has finished its stream in order:
it SHALL report that the peer has finished once everything received has
been pulled, SHALL go on sending what the caller queues, and SHALL end the
connection as orderly when the caller has shut its own side down. Without
half-close, which is the default, the end of the peer's stream SHALL end
the connection.

#### Scenario: Request, shutdown, reply
- **WHEN** a peer sends a request, shuts its write side down and waits, and the caller answers after pulling the request
- **THEN** the caller is told the peer has finished, its answer is sent and received by the peer, and after the caller's shutdown the connection is reported closed as orderly

#### Scenario: Default
- **WHEN** the same peer talks to a connection without half-close
- **THEN** the connection is reported closed as orderly after the request has been pulled, and a send after that is refused as not up

#### Scenario: End inside a frame
- **WHEN** a peer of a half-close connection ends its stream inside a frame, or without a TLS close alert
- **THEN** the connection is reported closed as a reset, and the peer is not reported as finished

#### Scenario: Send fails after the peer finished
- **WHEN** the peer has finished and then resets the connection while the caller's answer is being sent
- **THEN** the connection is reported closed as a reset, once

#### Scenario: Never answered
- **WHEN** the peer has finished and the caller neither sends, shuts down nor closes, with an idle timeout set
- **THEN** the connection is reported closed as timed out when the timeout passes

#### Scenario: Close racing with the answer
- **WHEN** the caller closes a half-closed connection while its answer is in flight
- **THEN** the slot is freed once, after the send has ended, and no event names the connection

#### Scenario: Both shut down
- **WHEN** the caller shut its side down before the peer finished
- **THEN** the connection is reported closed as orderly when the peer finishes, without the peer being reported as finished first

### Requirement: Errors that say what is wrong
The TCP set SHALL answer a call on a connection that exists but is not
open for what was asked (a send or a shutdown before it is up or after
its end, a send after the caller's shutdown) with an error that says so, distinct
from the error for a handle that names no connection, and SHALL accept
exactly one frame for a refused connection.

#### Scenario: Shutdown before up
- **WHEN** the write side of a connection that is still connecting is shut down
- **THEN** the call fails as not up, and the handle stays valid

#### Scenario: Second frame for a refused connection
- **WHEN** two frames are sent to a refused connection
- **THEN** the first is queued and the second is refused

### Requirement: Listener slots reused only when quiet
The TCP set SHALL give a listener slot to a new listener only when the
previous listener's close has completed and its accept has made its last
report.

#### Scenario: Listen right after closing
- **WHEN** a listener is closed and another is opened at once, repeatedly, while connections arrive and with the close's and the accept's reports in either order
- **THEN** each listener has at most one accept in flight, and every accepted connection is announced by the listener that accepted it or closed unannounced

### Requirement: Transport conformance
Every transport SHALL pass one conformance suite stating what the set
relies on: one report per operation, a close reported once, nothing
reported for a slot after its close and its operations have ended, no
connection placed in a slot that still owes a report, a busy refusal
that changes nothing, and a TLS handshake reported once, also when its
slot is closed under it.

#### Scenario: Kernel transport on three backends
- **WHEN** the conformance suite runs against the kernel transport on the simulator, the portable backend and the ring
- **THEN** it passes on each

### Requirement: Deterministic simulation under faults
The TCP set SHALL run in the simulator under seeded network faults, and for
each seed SHALL produce the same trace hash on every run; under every fault
each frame SHALL be delivered exactly once and in order, or its connection
SHALL be reported closed, never silently lost or duplicated.

#### Scenario: Same seed, same trace
- **WHEN** a simulated exchange with resets, partial reads and delays runs twice with the same seed
- **THEN** both runs produce the same trace hash

#### Scenario: Faults across many seeds
- **WHEN** request-and-response traffic runs over one thousand seeds with resets, short writes, partial reads, delays and peer restarts
- **THEN** in every run each frame is delivered once and in order or its connection is reported closed, and no allocation is reported after build

#### Scenario: Corruption under TLS
- **WHEN** bits are flipped on a TLS connection in the simulator
- **THEN** the connection is closed with an error and no corrupted frame is delivered

### Requirement: Storage and network faults together
The simulator SHALL run a TCP set and file I/O on one backend under network
faults and power loss in the same seeded run.

#### Scenario: Power loss mid-turn
- **WHEN** power is lost at a seeded point while frames are received, written from their extents and forwarded, over one thousand seeds
- **THEN** after recovery every frame that was answered is in the log, and no extent was reused while a write or send still held it

### Requirement: Packet-level simulation for user-space transports
The simulator SHALL offer a packet device with seeded loss, reordering,
duplication and delay, so that a transport with its own TCP can be tested
deterministically behind the same API.

#### Scenario: Packet faults are reproducible
- **WHEN** a packet stream crosses the device twice with the same seed and fault rates
- **THEN** the same packets are lost, reordered, duplicated and delayed both times

### Requirement: Defaults chosen by measurement
The TCP set's defaults SHALL be chosen by recorded benchmarks: the receive
mode, receive buffer size, zero-copy threshold, send segment size, log
sync method and turn wait strategy that
measure best for request-and-response at UltraGraphDB's frame sizes and
for bulk transfer, recorded in `bench/results/` against an `epoll`
baseline on the same host.

#### Scenario: Results recorded
- **WHEN** the defaults are set
- **THEN** `bench/results/` holds latency (p50, p99, p999), throughput per core and connection-scale results for each candidate value, with the host and kernel named

### Requirement: Zero-copy sends not slower than copied sends
Zero-copy sends over a network card SHALL NOT, with default settings,
carry fewer bytes a second than copied sends of the same frames, for any
frame length at or above the zero-copy threshold, compared by the medians
of repeated runs.

#### Scenario: 64 KiB frames over a card
- **WHEN** one thread sends 64 KiB frames on four connections to another machine with default settings, with zero-copy sends and without
- **THEN** over five runs each, the median with zero-copy sends is no less than nine tenths of the median without

### Requirement: A frame's body at a block boundary
Header framing SHALL offer to receive a frame, in direct mode, so that its
body, not its header, starts at a block boundary of the frame's memory,
with and without TLS, so that the body can go to a direct-I/O write from
where it lies. Without TLS no byte of the body SHALL be moved after it is
received.

#### Scenario: Body written as it lies
- **WHEN** a frame with a 256-byte header is received in direct mode with the body asked for at a block of 4,096 bytes
- **THEN** the frame starts 3,840 bytes into its memory and its body at a multiple of the block

#### Scenario: Where it cannot be given
- **WHEN** a connection is opened with the option in ring mode, or with frame memory that cannot hold the lead and the largest frame or is not whole blocks
- **THEN** opening it fails

### Requirement: Where a frame's bytes lie, by its kind
A received frame SHALL say where its bytes lie only when they lie in one
run of memory: a frame in frame memory or lent from the buffer ring SHALL
give its span, and a frame lent in pages of a receive area SHALL give
none and be read through its pages. Likewise a frame SHALL give its frame
memory only when it is in frame memory.

#### Scenario: A frame in frame memory
- **WHEN** a frame received in direct mode is asked for its span and its frame memory
- **THEN** it gives both, as before

#### Scenario: A frame in pages
- **WHEN** a frame lent in pages is asked for its span or its frame memory
- **THEN** it gives neither, and its pages give its bytes in order

