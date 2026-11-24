## ADDED Requirements

### Requirement: AF_XDP transport behind the same API
`longshore_tcp` SHALL offer a transport that runs TCP over AF_XDP sockets
with a user-space TCP stack, usable through the same `Net` API as the
kernel transport, refusing Unix-domain addresses.

#### Scenario: Same program, other transport
- **WHEN** a program written against `Net` with the kernel transport is built with the AF_XDP transport instead
- **THEN** it exchanges the same frames with the same peers without other changes

#### Scenario: Unix address
- **WHEN** a Unix-domain address is given to the AF_XDP transport
- **THEN** the call fails with an error saying the transport does not carry Unix sockets

### Requirement: Interoperability with Linux TCP
The user-space TCP stack SHALL interoperate with Linux TCP peers, including
window scaling, selective acknowledgement, timestamps and loss recovery.

#### Scenario: Long transfer under loss
- **WHEN** 100 GB are transferred each way between the AF_XDP transport and a Linux TCP peer while one packet in ten thousand is dropped and some are reordered
- **THEN** the received bytes match the sent bytes on both sides

### Requirement: One move on receive, none on transmit, memory at build
The AF_XDP transport SHALL use the backend's buffer pool as its UMEM,
allocated when built, SHALL NOT allocate afterwards, SHALL move received
payload exactly once, into block-aligned frame memory, and SHALL transmit
frames from frame memory without copying them.

#### Scenario: Receive, log, replicate over AF_XDP
- **WHEN** a frame received over the AF_XDP transport is written to a file with direct I/O and sent to three peers
- **THEN** its payload was moved once on receive and not again

#### Scenario: Allocation guard
- **WHEN** traffic runs over the AF_XDP transport under `alloc_guard` after build
- **THEN** no allocation is reported

### Requirement: Refusal instead of fallback
Building the AF_XDP transport SHALL fail with an error naming what is
missing when the NIC driver lacks zero-copy support, privileges are
missing, or the kernel lacks a feature the transport uses; copy mode SHALL
run only when configured.

#### Scenario: No zero-copy support
- **WHEN** the transport is built on a NIC whose driver has no AF_XDP zero copy, without copy mode configured
- **THEN** building fails and the error names the driver and the missing support

### Requirement: Each connection on its own core's queue
Every packet of a connection SHALL arrive on the NIC queue of the core that
owns the connection.

#### Scenario: Many connections across cores
- **WHEN** eight cores each open one hundred connections to peers
- **THEN** no core receives a packet of a connection owned by another core

### Requirement: Measured on 200 and 300 Gb/s NICs
The AF_XDP transport's throughput, CPU per GB and latency SHALL be measured
on physical hosts with 200 and 300 Gb/s NICs against the kernel transport
(with and without io_uring zero-copy receive where the kernel has it), and
the results and the tuning that produced them SHALL be recorded in
`bench/results/`.

#### Scenario: Results recorded
- **WHEN** the backend is declared ready
- **THEN** `bench/results/` holds, per NIC: GB/s and cores at line rate, CPU per GB, and p50/p99 request latency for both transports, with host, NIC, driver, kernel and settings named

### Requirement: One wait point
A thread driving the AF_XDP transport and a ring SHALL sleep only in the
ring, with the transport's socket armed there, and only when no source has
work in flight.

#### Scenario: Idle then traffic
- **WHEN** the thread is asleep with nothing in flight and a packet arrives
- **THEN** the thread wakes and delivers the frame, and while idle it used no CPU

### Requirement: Stated state machines, checked
The AF_XDP transport SHALL behave as the state tables of its design say,
for every state and event of a TCP connection, a UMEM chunk and a queue's
socket, and its invariants SHALL hold after every step of any sequence of
calls, packets, timers and packet faults.

#### Scenario: Random operations and packet faults
- **WHEN** seeded random calls on both ends and seeded loss, reordering, duplication and delay between them run against the smallest UMEM, rings and windows
- **THEN** every invariant holds after every step, and each end delivers what the other sent, once and in order

#### Scenario: Every cell
- **WHEN** the model test has run its seeds
- **THEN** every cell of the TCP state table was reached or is asserted unreachable

#### Scenario: Failing seed
- **WHEN** a run fails
- **THEN** it reports its seed and step count, and replaying them fails in the same way

### Requirement: A slot's next connection is never reached by its predecessor
Closing a connection SHALL free its slot in the same turn, and packets of
the closed connection that arrive afterwards SHALL be answered from a
fixed linger table and SHALL NOT reach the connection that has the slot
next.

#### Scenario: Slot taken again at once
- **WHEN** a connection is closed, its slot is given to a new connection to the same peer, and delayed and duplicated packets of the old connection then arrive
- **THEN** the new connection delivers only its own bytes, and the old connection's peer sees an orderly close

#### Scenario: Linger table full
- **WHEN** more connections are closed in order than the linger table holds
- **THEN** the ones over the limit are reset, and nothing allocates

### Requirement: Memory held until acknowledged
Memory a segment was transmitted from SHALL stay held until the segment is
acknowledged or its connection has ended, and a send SHALL be reported to
the set as ended only then.

#### Scenario: Loss after transmit
- **WHEN** a frame lent with `send_lent` is transmitted, released by the caller, and its packets are then lost
- **THEN** the retransmission carries the same bytes, and the frame's memory is free only after the acknowledgement

### Requirement: Exhaustion without loss
The AF_XDP transport SHALL keep every connection correct when a pool of
its own is empty (free chunks, the transmit and completion rings,
reassembly, frame memory, the send and congestion windows, accept slots,
the linger table, a neighbour entry), and SHALL resume by itself when
there is room.

#### Scenario: Zero window and back
- **WHEN** the caller holds every frame buffer so that the receive window closes, and then releases them
- **THEN** a window update goes out in the same turn and the transfer continues without waiting for the peer's probe

#### Scenario: Every pool
- **WHEN** each pool in turn is the only one exhausted during a transfer
- **THEN** the transfer completes after the pool has room again, and the received bytes match the sent bytes

### Requirement: Transport conformance
The AF_XDP transport SHALL pass the same transport conformance suite as
the kernel transport.

#### Scenario: Close racing with packets
- **WHEN** a connection is closed while segments of it are in the transmit ring, unacknowledged, and arriving
- **THEN** its close is reported once, nothing is reported for the slot afterwards, and every chunk and extent it used is accounted for
