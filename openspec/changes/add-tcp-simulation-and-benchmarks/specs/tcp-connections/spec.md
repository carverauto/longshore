## ADDED Requirements

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
open for what was asked (a send or a shutdown before it is up, after its
end, or after the caller's shutdown) with an error that says so, distinct
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
connection placed in a slot that still owes a report, and a busy refusal
that changes nothing.

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
