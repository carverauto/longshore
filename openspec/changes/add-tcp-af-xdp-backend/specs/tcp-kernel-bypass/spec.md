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
