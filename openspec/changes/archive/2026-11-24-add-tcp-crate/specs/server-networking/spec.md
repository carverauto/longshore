## ADDED Requirements

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
