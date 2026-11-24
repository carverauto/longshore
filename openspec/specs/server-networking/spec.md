# server-networking Specification

## Purpose
Networking for servers on `Io`: operations with several completions,
accept that stays armed, receive from a buffer ring, socket options,
Unix-domain sockets, one plain-or-TLS stream for clients and servers, and
TLS as a build feature.
## Requirements
### Requirement: Operations with several completions
A completion SHALL say whether more completions will follow for its token.
An operation that stays armed SHALL end with exactly one completion that
says no more will follow, which MAY carry a result as any other does, and
no completion SHALL be dropped.

#### Scenario: Armed operation cancelled
- **WHEN** an armed receive is cancelled
- **THEN** it delivers one last completion marked as final and none after it

#### Scenario: Operation that completes once
- **WHEN** a send completes
- **THEN** its completion is marked as final

#### Scenario: Ended with bytes
- **WHEN** an armed receive ends while delivering bytes
- **THEN** its final completion names the buffer and the count, and the bytes are those the peer sent

#### Scenario: More completions than one turn takes
- **WHEN** armed operations produce more completions than the caller's buffer holds
- **THEN** every one of them is delivered over the following turns

#### Scenario: Existing traces
- **WHEN** a simulation that uses no armed operation runs with the seed it ran with before this change
- **THEN** its trace hash is the same

### Requirement: Accept that stays armed
The I/O layer SHALL accept connections on a listener with one submission,
placing each in a free slot of a range reserved for it and reporting the
slot in the completion.

#### Scenario: Several connections
- **WHEN** three clients connect to a listener with one accept armed
- **THEN** three completions arrive for its token, each with a different slot of the range, and the accept stays armed

#### Scenario: Range full
- **WHEN** a connection arrives and every slot of the range is in use
- **THEN** that connection is dropped and the accept ends with a final completion carrying the table-full error

#### Scenario: Armed again
- **WHEN** an accept that ended is armed again after a slot was closed
- **THEN** the next connection is accepted

### Requirement: Receive from a buffer ring
The I/O layer SHALL receive on a socket with one submission, taking a
buffer from a ring of buffers only when bytes arrive and reporting the
buffer and the byte count in each completion that carries bytes.

#### Scenario: Idle connections hold no buffer
- **WHEN** a thousand plain connections each have a receive armed and none has sent a byte
- **THEN** every buffer of the ring is free

#### Scenario: Bytes arrive
- **WHEN** a peer sends bytes on a socket with a receive armed
- **THEN** a completion names a buffer holding those bytes and the receive stays armed

#### Scenario: Buffer given back
- **WHEN** the caller gives a buffer back
- **THEN** a later completion may name the same buffer

#### Scenario: Ring empty
- **WHEN** bytes arrive and no buffer is free
- **THEN** the receive ends with a final completion carrying the no-buffers error, and the bytes are delivered after the caller gives a buffer back and arms again

#### Scenario: Peer closes
- **WHEN** the peer closes a socket with a receive armed
- **THEN** the receive ends with a final completion of zero that names no buffer, and no buffer is lost from the ring

### Requirement: Closing with an armed operation
A close SHALL end the armed operations on its socket, and the peer of a
connected socket SHALL see the connection end.

#### Scenario: Peer sees the end
- **WHEN** a server closes a connection that has a receive armed
- **THEN** the receive delivers a final completion and the client's next receive completes with zero

### Requirement: Socket options
The backend SHALL set no-delay and keep-alive, when configured, on every
TCP socket it connects or accepts, on Linux 6.1 and later, and SHALL fail a
connect on which it cannot set them.

#### Scenario: Accepted socket
- **WHEN** no-delay is configured and a connection is accepted on Linux 6.1
- **THEN** reading the option from that socket shows it set

#### Scenario: Connected socket
- **WHEN** keep-alive is configured and a connect succeeds
- **THEN** reading the option from that socket shows it set

### Requirement: Unix-domain sockets
The I/O layer SHALL listen on and connect to Unix-domain stream sockets by
path, with the same send, receive and close operations as TCP.

#### Scenario: Echo over a path
- **WHEN** the echo scenario runs over a Unix-domain socket
- **THEN** every byte sent is received

#### Scenario: No listener
- **WHEN** a connect names a path nothing listens on
- **THEN** it completes with an error and the slot is free

### Requirement: One stream for clients and servers
The plain-or-TLS byte stream over `Io` SHALL be provided by a crate that
depends on no HTTP code, for both the connecting and the accepting side.

#### Scenario: Server side with TLS
- **WHEN** a server accepts a connection and runs the TLS handshake through the stream
- **THEN** it reads the plaintext the client sent

#### Scenario: HTTP client unchanged
- **WHEN** the HTTP client's tests run after the move
- **THEN** they pass without change to their expectations

### Requirement: TLS as a build feature
TLS SHALL be built only when a TLS provider feature (`aws-lc`, `ring` or `rustls`) is
on, with `aws-lc` on by default, and the `longshore` crate built with
none SHALL NOT depend on `longshore_tls`, `rustls`, `aws-lc-sys` or
`aws-lc-rs`.

#### Scenario: Plain build
- **WHEN** the dependencies of `longshore` are listed with default features off, leaving out those used only by tests
- **THEN** the list holds none of `longshore_tls`, `rustls`, `aws-lc-sys` and `aws-lc-rs`

#### Scenario: TLS asked for in a plain build
- **WHEN** a client built without a provider feature is configured to use TLS
- **THEN** building the client fails with an error that names the feature

#### Scenario: Tests without the feature
- **WHEN** the workspace's tests run with no provider feature on
- **THEN** the tests that need no TLS pass

### Requirement: Every backend
The ring, the portable backend and the simulator SHALL behave the same for
the armed accept, the armed receive, socket options and Unix-domain
sockets.

#### Scenario: Shared tests
- **WHEN** the shared networking tests run against the three backends
- **THEN** each passes the same tests

#### Scenario: Faults in the simulator
- **WHEN** an armed receive runs in the simulator with resets and partial reads injected
- **THEN** every byte delivered before a reset is reported once and the receive ends with a final completion

### Requirement: Shutting down one direction
The `Io` trait SHALL shut down the read side, the write side or both of a
connected socket without closing its slot, on the ring, the portable
backend and the simulator.

#### Scenario: Half-close
- **WHEN** one side shuts down its write side after sending
- **THEN** the peer reads all the data and then end of stream, and can still send back

### Requirement: Peer address of an accepted connection
The `Io` trait SHALL report the address of the peer of a connection
accepted with a single accept operation.

#### Scenario: Peer address
- **WHEN** a client connects from a known address and the server accepts it
- **THEN** the server reads the client's address and port for that slot

### Requirement: Keepalive timing
The `Io` trait SHALL set the keepalive idle time, probe interval and probe
count on TCP sockets created from then on, where the platform supports
them.

#### Scenario: Timings applied
- **WHEN** keepalive is enabled with an idle time, an interval and a count before a connect
- **THEN** the connected socket carries those values

### Requirement: Shared port and backlog
Listening SHALL accept `SO_REUSEPORT` and a backlog, so that several rings
can listen on one address.

#### Scenario: Two listeners, one port
- **WHEN** two rings listen on the same address with port sharing
- **THEN** both listens succeed and each accepts some of the incoming connections

### Requirement: Stream receives from the buffer ring
A stream SHALL accept received bytes delivered in buffer-ring buffers, for
plain and TLS connections; with TLS it SHALL open records into the caller's
destination, carry a record that continues in the next ring buffer, and
leave no plaintext in a ring buffer.

#### Scenario: TLS records across ring buffers
- **WHEN** a TLS peer sends data whose records straddle the ring buffers an armed receive delivers
- **THEN** the stream yields the same plaintext as with ordinary receives, and each ring buffer can be given back as soon as the stream has consumed it

