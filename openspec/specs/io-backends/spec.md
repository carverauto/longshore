# io-backends Specification

## Purpose
The contract every `Io` backend keeps (completion, cancellation, closing),
one engine shared by the simulator and the portable `kqueue`/`epoll`
backend, and the client running on hosts without `io_uring`.

## Requirements
### Requirement: Completion contract
Every I/O backend SHALL deliver exactly one final completion for each
operation it accepts, SHALL refuse an operation with `Busy` when its fixed
capacity is in use, and SHALL report results as a byte count, zero, or a
negated Linux error number.

#### Scenario: One completion
- **WHEN** an operation is accepted
- **THEN** exactly one final completion carrying its token is delivered

#### Scenario: Capacity in use
- **WHEN** as many operations are in flight as the limits allow and another is submitted
- **THEN** it is refused with `Busy` and no completion is delivered for it

#### Scenario: Error numbers on macOS
- **WHEN** a peer resets a connection and a receive is in flight on the portable backend on macOS
- **THEN** the completion carries the Linux value for a connection reset

#### Scenario: Unknown error
- **WHEN** a system call on the portable backend fails with an error the translation does not know
- **THEN** the completion carries the Linux value for an I/O error

### Requirement: Cancellation
A cancel SHALL name the operation in flight under its target token at the
moment the cancel is submitted, on every backend.

#### Scenario: Cancelled while waiting
- **WHEN** a receive is waiting and a cancel names its token
- **THEN** the receive completes with the cancelled error and the cancel completes with zero

#### Scenario: Nothing to cancel
- **WHEN** a cancel names a token with no operation in flight
- **THEN** the cancel completes with a negative result

#### Scenario: Token used again
- **WHEN** a cancel is submitted and a new operation is then submitted under the same token
- **THEN** the new operation is not cancelled

### Requirement: Closing a socket
A close SHALL give every other operation in flight on its socket its final
completion without waiting for the peer, on every backend.

#### Scenario: Receive in flight
- **WHEN** a socket is closed while a receive is in flight on it and the peer sends nothing
- **THEN** the receive completes with a negative result and the close completes

#### Scenario: Accept in flight
- **WHEN** a listener is closed while an accept is in flight on it
- **THEN** the accept completes with a negative result

#### Scenario: Empty slot
- **WHEN** a slot that holds no socket is closed
- **THEN** the close completes once

### Requirement: One engine for the simulator and the portable backend
The simulator and the portable backend SHALL share one implementation of
pending operations, cancellation, timers, back pressure and completion
delivery, and differ only in what attempts an operation, what draws from
the seed, and how they tell time and wait.

#### Scenario: Existing seeds
- **WHEN** a simulation that ran before this change, and closes no socket with operations in flight, is run again with the same seed and inputs
- **THEN** its trace hash is the same

#### Scenario: Seeds with delays
- **WHEN** such a simulation runs with the delay fault on
- **THEN** its trace hash is the same as before this change

#### Scenario: Same seed twice
- **WHEN** a simulation is run twice with one seed
- **THEN** both runs deliver the same completions in the same order

### Requirement: Portable backend
The I/O layer SHALL provide a backend that performs every socket operation
of the `Io` trait with non-blocking system calls, waiting with kqueue on
macOS and with epoll on Linux, on the one thread that calls `tick`, and
that reads a file on that thread.

#### Scenario: Echo on macOS
- **WHEN** the echo scenario is run on the portable backend on macOS over loopback
- **THEN** every byte sent is received and the scenario ends without error

#### Scenario: Both directions waiting on Linux
- **WHEN** a send and a receive are both waiting on one socket on the portable backend on Linux and the socket becomes readable
- **THEN** the receive completes and the send completes when the socket becomes writable

#### Scenario: Zero-copy flag
- **WHEN** a send asks for zero copy on the portable backend
- **THEN** the bytes are sent and the send completes once

#### Scenario: Send to a closed peer
- **WHEN** a send is made on a connection the peer has reset
- **THEN** the send completes with a negative result and the process receives no signal

#### Scenario: Woken from another thread
- **WHEN** `tick` is waiting and another thread signals the backend's waker
- **THEN** a pending `AwaitWake` completes

#### Scenario: No allocation after start
- **WHEN** the portable backend runs the echo scenario under the allocator that aborts on allocation
- **THEN** the run completes

### Requirement: Readiness is never lost
The portable backend SHALL NOT lose or indefinitely delay the completion of
any operation because of a report that was stale, a completion buffer that
was full, or a caller that kept submitting.

#### Scenario: Stale report among real ones
- **WHEN** one wait reports several descriptors and an attempt on one of them finds nothing to do
- **THEN** the operations on the others complete in that turn and the first stays pending

#### Scenario: Stale report alone
- **WHEN** a descriptor is reported ready, the attempt finds nothing, and data arrives later
- **THEN** the operation completes when the data arrives

#### Scenario: Completion buffer full
- **WHEN** more operations are ready in one turn than the completion buffer holds
- **THEN** the rest complete in later turns without another report from the kernel

#### Scenario: Caller keeps submitting
- **WHEN** each turn submits an operation that completes at once while a receive waits on a socket that has data
- **THEN** the receive completes

### Requirement: Shared backend tests
One set of tests of the `Io` contract SHALL run against the ring, the
portable backend and the simulator.

#### Scenario: Continuous integration
- **WHEN** a pull request is checked
- **THEN** the shared tests run against the simulator, against the portable backend on macOS and on Linux, and against the ring

#### Scenario: No Linux host configured
- **WHEN** the local test script runs and no Linux host is configured for the real-kernel checks
- **THEN** the script fails and says that the ring was not exercised

### Requirement: Client on a host without the ring
The threaded client SHALL start on macOS using the portable backend, and on
Linux SHALL use the ring unless its builder asks for the portable backend.

#### Scenario: Client on macOS
- **WHEN** a client is built on macOS and stores then fetches an object from a local test server
- **THEN** both operations succeed

#### Scenario: Ring refused on Linux
- **WHEN** the kernel refuses the ring and the builder did not ask for the portable backend
- **THEN** building the client fails with an error that says why

#### Scenario: Pinning on macOS
- **WHEN** a client is built on macOS with threads pinned to cores
- **THEN** building the client fails with a setting error

