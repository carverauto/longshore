## ADDED Requirements

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

### Requirement: Packet-level simulation for user-space transports
The simulator SHALL offer a packet device with seeded loss, reordering,
duplication and delay, so that a transport with its own TCP can be tested
deterministically behind the same API.

#### Scenario: Packet faults are reproducible
- **WHEN** a packet stream crosses the device twice with the same seed and fault rates
- **THEN** the same packets are lost, reordered, duplicated and delayed both times

### Requirement: Defaults chosen by measurement
The TCP set's defaults SHALL be chosen by recorded benchmarks: the receive
mode, receive buffer size, zero-copy threshold and send segment size that
measure best for request-and-response at UltraGraphDB's frame sizes and
for bulk transfer, recorded in `bench/results/` against an `epoll`
baseline on the same host.

#### Scenario: Results recorded
- **WHEN** the defaults are set
- **THEN** `bench/results/` holds latency (p50, p99, p999), throughput per core and connection-scale results for each candidate value, with the host and kernel named
