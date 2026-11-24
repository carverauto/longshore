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
- **WHEN** a peer's last frame and the end of its stream arrive while every frame buffer is held, on a plain and on a TLS connection, in ring and in direct mode
- **THEN** the frame is delivered once frame memory is released, and the end is reported after it as orderly

#### Scenario: Shared pool empty
- **WHEN** a send is refused because the shared segment pool is empty while the connection's own queue has room
- **THEN** the connection is told it can send once a segment is returned, also when the connection told before it has gone

#### Scenario: Every pool
- **WHEN** each pool in turn is the only one exhausted during an exchange
- **THEN** the exchange completes after the pool has room again, and every byte arrives once and in order

### Requirement: End of stream after everything received
The TCP set SHALL offer every frame received before the end of a stream
before it reports the end, and SHALL report the end as orderly only when
the stream ended between frames and, with TLS, after a close alert.

#### Scenario: Data and end in one turn
- **WHEN** frames, a partial frame and the end of the stream arrive in the same turn
- **THEN** the whole frames are delivered and the end is reported as a reset

#### Scenario: Frame that is only a header
- **WHEN** a header announcing an empty body arrives in two pieces and the stream then ends
- **THEN** the frame is delivered and the end is reported as orderly

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
