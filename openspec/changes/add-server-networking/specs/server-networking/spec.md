## ADDED Requirements

### Requirement: Operations with several completions
A completion SHALL say whether more completions will follow for its token,
and an operation that stays armed SHALL end with exactly one completion
that says no more will follow.

#### Scenario: Armed operation cancelled
- **WHEN** an armed receive is cancelled
- **THEN** it delivers one last completion marked as final and none after it

#### Scenario: Operation that completes once
- **WHEN** a send completes
- **THEN** its completion is marked as final

### Requirement: Accept that stays armed
The I/O layer SHALL accept connections on a listener with one submission,
placing each in a free slot of a range reserved for it and reporting the
slot in the completion.

#### Scenario: Several connections
- **WHEN** three clients connect to a listener with one accept armed
- **THEN** three completions arrive for its token, each with a different slot, and the accept stays armed

#### Scenario: Range full
- **WHEN** a connection arrives and every slot of the range is in use
- **THEN** the accept ends with a final completion carrying the no-space error

### Requirement: Receive from a buffer ring
The I/O layer SHALL receive on a socket with one submission, taking a
buffer from a ring of buffers only when bytes arrive and reporting the
buffer and the byte count in each completion.

#### Scenario: Idle connections hold no buffer
- **WHEN** a thousand connections each have a receive armed and none has sent a byte
- **THEN** every buffer of the ring is free

#### Scenario: Bytes arrive
- **WHEN** a peer sends bytes on a socket with a receive armed
- **THEN** a completion names a buffer holding those bytes and the receive stays armed

#### Scenario: Buffer given back
- **WHEN** the caller gives a buffer back
- **THEN** a later completion may name the same buffer

#### Scenario: Ring empty
- **WHEN** bytes arrive and no buffer is free
- **THEN** the receive ends with a final completion carrying the no-buffers error and no byte is lost

#### Scenario: Peer closes
- **WHEN** the peer closes a socket with a receive armed
- **THEN** the receive ends with a final completion of zero

### Requirement: Socket options
The backend SHALL set no-delay and keep-alive, when configured, on every
TCP socket it connects or accepts, on Linux 6.1 and later.

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
TLS SHALL be a cargo feature that is on by default, and a build without it
SHALL compile no TLS code and no AWS-LC.

#### Scenario: Plain build
- **WHEN** the workspace is built with the TLS feature off
- **THEN** the dependency tree holds neither `longshore_tls` nor `aws-lc-sys`

#### Scenario: TLS asked for in a plain build
- **WHEN** a client built without the feature is configured to use TLS
- **THEN** building the client fails with an error that names the feature

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
