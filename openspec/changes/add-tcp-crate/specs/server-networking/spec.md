## ADDED Requirements

### Requirement: Shutting down one direction
The `Io` trait SHALL shut down the read side, the write side or both of a
connected socket without closing its slot, on the ring, the portable
backend and the simulator.

#### Scenario: Half-close
- **WHEN** one side shuts down its write side after sending
- **THEN** the peer reads all the data and then end of stream, and can still send back

### Requirement: Peer address of an accepted connection
The `Io` trait SHALL report the address of the peer of an accepted
connection.

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
