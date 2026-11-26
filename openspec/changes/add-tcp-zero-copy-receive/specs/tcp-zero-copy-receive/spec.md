## ADDED Requirements

### Requirement: Zero-copy receive as a receive mode
`longshore_tcp` SHALL offer a receive mode on the ring backend, for plain
TCP connections with header framing, in which a frame whose blocks arrive
lying in whole pages of a receive area registered at build is lent as
those pages without a copy, and every other frame is delivered in frame
memory as direct mode delivers it, with the same states and events as
direct mode.

#### Scenario: The same frames and events as direct mode
- **WHEN** a program receives a stream of frames in the mode that it also receives in direct mode
- **THEN** it pulls the same frames in the same order and sees the same events, whatever the pieces the stream arrived in

#### Scenario: A frame that starts a packet of whole pages
- **WHEN** a frame of 1 MiB arrives starting at the start of a page, in packets that each carry a whole number of pages
- **THEN** it is lent as its whole pages and a tail block, and the set copied only its header and its last, partial block

#### Scenario: A frame that does not line up
- **WHEN** a frame starts inside a page, or a short packet arrives inside it
- **THEN** it is delivered in frame memory, moved once, and its bytes are the bytes sent

### Requirement: Where the mode is refused
The mode SHALL be refused, with an error and without a fallback: when the
backend is built, where the kernel, the driver, the process's privileges
or the card's settings cannot provide zero-copy receive, naming which; and
when a connection or listener is made, with TLS, on a Unix-domain address,
without header framing, on a port shared between listeners, or on a
connection set not sized for it.

#### Scenario: Kernel without the feature
- **WHEN** a backend with a receive area is built on a kernel without zero-copy receive
- **THEN** the build fails with an error naming the kernel feature

#### Scenario: Card not splitting headers
- **WHEN** a backend with a receive area is built on an interface whose queue does not split headers from payload
- **THEN** the build fails with an error naming the card setting

#### Scenario: With TLS
- **WHEN** a connection is opened in the mode with TLS
- **THEN** opening it fails

### Requirement: Only whole pages held, under a bound the kernel keeps
The mode SHALL hold past a turn only whole pages that each hold one whole
block of one frame lent in pages, never more of them than the page budget
sized at build, and SHALL arm a connection's receive only for the rest of
its header or of its frame, for which it already has a place, so that no
received byte waits for memory.

#### Scenario: Many connections part-way through large frames
- **WHEN** more connections are part-way through frames than there are frame buffers, with the smallest page budget
- **THEN** every frame is delivered once frame memory is released, and no connection waits for ever

#### Scenario: A connection that is not pulled
- **WHEN** the caller does not pull one connection while others receive
- **THEN** that connection holds one frame and reads no further, and the others go on

#### Scenario: The page budget used up
- **WHEN** a frame lines up and the page budget has no room for it
- **THEN** it is delivered in frame memory, and nothing waits

### Requirement: Frames in pages held by count and returned once
A frame lent in pages SHALL stay readable until its last hold is released
(the caller's, and one for each file write that took one), SHALL be
released exactly once per hold, with a stale or second release doing
nothing, and its pages SHALL be returned to the kernel when the last hold
goes, each piece exactly once.

#### Scenario: Release returns the pages
- **WHEN** every frame is released after a run of frames in and out of step
- **THEN** no piece is out, the page budget is whole, and the set is idle

#### Scenario: A copy of a frame released twice
- **WHEN** a frame in pages is released and a copy of it is released again
- **THEN** the second release does nothing, and no other frame's pages are returned

#### Scenario: Closing with frames lent
- **WHEN** a connection closes while a frame of it in pages is still held
- **THEN** the frame stays readable until released and its pages are returned then

### Requirement: A frame in pages written without a copy
The backend SHALL offer a vectored file write over spans of its pool, and
a frame lent in pages SHALL be writable with it to a file opened for
direct I/O from where it lies: its whole pages and its zero-padded tail
block.

#### Scenario: Received, written, synced
- **WHEN** a frame lent in pages is written to a file with direct I/O and synced, under a hold of its own
- **THEN** the file holds the frame's bytes padded with zeros to a whole block, and the set copied only its header and its last block

### Requirement: Shortages without loss
The mode SHALL NOT lose, repeat or reorder a byte when frame memory, the
page budget, room in the refill ring or the completion ring, or the
kernel's free pages run short: a shortage slows or redirects receiving,
and each is counted where the program can see it.

#### Scenario: Refill ring smaller than what is returned at once
- **WHEN** more pieces are returned in a turn than the refill ring holds
- **THEN** all of them reach the kernel, none twice

#### Scenario: A receive ended by a shortage
- **WHEN** a receive of the mode ends because the completion ring or the kernel's free pages ran out
- **THEN** it is armed again for the rest of its credit and the frame is delivered whole

### Requirement: Memory at build
The mode SHALL allocate the area, its refill ring, the runs, the tail
blocks and every table of its own when the backend and the connection set
are built, and nothing afterwards.

#### Scenario: Allocation guard
- **WHEN** traffic runs in the mode under `alloc_guard` after build
- **THEN** no allocation is reported

### Requirement: Simulated on every host
The simulator SHALL deliver receives of the mode as pieces of pages with
seeded lengths, pages split between pieces, frames in and out of step,
the kernel's copy path and each shortage, SHALL fail a run on a piece
returned twice or read after its return, and the model test and the
transport conformance suite SHALL run in the mode.

#### Scenario: Same seed, same trace
- **WHEN** the model test runs twice in the mode with one seed
- **THEN** both runs produce the same trace hash

#### Scenario: A read after return is seen
- **WHEN** a fault is put into the set that reads a piece after returning it
- **THEN** the model test fails

### Requirement: Measured against the copying modes
The mode's CPU cost per byte SHALL be measured on a card against ring and
direct mode, counting the cores that handle the queue's packets, for
frames written to a log as they lie, frames read in place and frames that
do not line up, with the share of frames lent in pages, and the results
SHALL be recorded in `bench/results/` with the card settings used.

#### Scenario: Result on record
- **WHEN** the change's tasks are marked done
- **THEN** `bench/results/` holds the comparison and `docs/tcp.md` the settings and the hosts it ran on
