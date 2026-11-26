## ADDED Requirements

### Requirement: Zero-copy receive as a receive mode
`longshore_tcp` SHALL offer a receive mode on the ring backend in which
the kernel does not copy received payload: payload arrives in chunks of a
memory area registered at build for one receive queue of one interface,
through the same `Net` API and with the same states and events as the
other receive modes.

#### Scenario: Same program, other mode
- **WHEN** a program written against `Net` chooses the zero-copy receive mode for its listener instead of ring mode
- **THEN** it receives the same frames in the same order and sees the same events

#### Scenario: Not on the portable backend
- **WHEN** the mode is chosen on the portable backend
- **THEN** the call fails with an error saying the mode needs the ring backend

### Requirement: Frames lent without a copy, whole or in pieces
A frame that lies in one chunk SHALL be lent in place. A frame that spans
chunks SHALL be lent as its byte runs in order without copying them, and
SHALL be moved exactly once, into block-aligned frame memory, only when
the caller asks to hold it.

#### Scenario: Frame in one chunk
- **WHEN** a frame arrives within one chunk
- **THEN** its bytes are read from the chunk and no payload byte was copied

#### Scenario: Frame across chunks
- **WHEN** a frame of 1 MiB arrives across many chunks and the caller reads its pieces
- **THEN** the pieces together are the frame's bytes in order and no payload byte was copied

#### Scenario: Held for a direct write
- **WHEN** the caller holds a frame that spans chunks and writes it to a file with direct I/O
- **THEN** its payload was moved once, into frame memory, and its chunks were returned

### Requirement: Chunks accounted and returned
Every chunk of the area SHALL at any time be with the kernel, lent with
exactly one frame, or queued for return, and SHALL be returned to the
kernel when its frame is released, without a system call.

#### Scenario: Release returns chunks
- **WHEN** every frame is released after a run of mixed frame sizes
- **THEN** every chunk is with the kernel or queued for return and none is lost

#### Scenario: Closing with frames lent
- **WHEN** a connection closes while the caller still holds frames from it
- **THEN** the frames stay readable until released and their chunks are returned then

### Requirement: Exhaustion without loss
An area with no free chunk SHALL stop the flow of received data, not lose
or reorder it, and data SHALL flow again when frames are released.

#### Scenario: Slow consumer
- **WHEN** a caller holds frames until the area has no free chunk and then releases them
- **THEN** the sender was slowed, no byte was lost, and the stream continues in order

### Requirement: Memory at build
The area, its refill ring and their accounting SHALL be allocated when the
backend is built, sized from `Limits`, and nothing SHALL be allocated for
the mode afterwards.

#### Scenario: Allocation guard
- **WHEN** traffic runs in the zero-copy receive mode under `alloc_guard` after build
- **THEN** no allocation is reported

### Requirement: Refusal instead of fallback
Building a backend with the mode SHALL fail with an error naming what is
missing when the kernel, the driver, the process's privileges or the
card's settings cannot provide zero-copy receive, and SHALL NOT fall back
to a copying mode.

#### Scenario: Kernel without the feature
- **WHEN** the mode is asked for on a kernel without zero-copy receive
- **THEN** the build fails with an error naming the kernel feature

#### Scenario: Card not splitting headers
- **WHEN** the mode is asked for on an interface whose queue does not split headers from payload
- **THEN** the build fails with an error naming the card setting

### Requirement: Simulated on every host
The simulator SHALL deliver receives of the mode in chunks with seeded
boundaries and seeded exhaustion, and the model test and the transport
conformance suite SHALL run in the mode.

#### Scenario: Same seed, same trace
- **WHEN** the model test runs twice in the mode with one seed
- **THEN** both runs produce the same trace hash

### Requirement: Measured against the copying modes
The mode's CPU cost per byte received SHALL be measured on a card against
ring and direct mode, for frames consumed in place and for frames held for
a log write, and the results SHALL be recorded in `bench/results/` with the
card settings used.

#### Scenario: Result on record
- **WHEN** the change's tasks are marked done
- **THEN** `bench/results/` holds the comparison and `docs/tcp.md` the settings and the hosts it ran on
