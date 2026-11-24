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

