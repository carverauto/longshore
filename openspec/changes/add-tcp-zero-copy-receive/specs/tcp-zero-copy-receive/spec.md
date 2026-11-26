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
without header framing, with a body asked for at a block, on a port
shared between listeners, or on a connection set not sized for it; and
when a connection set is planned with pages for the mode and fewer frame
buffers than connections.

#### Scenario: Kernel without the feature
- **WHEN** a backend with a receive area is built on a kernel without zero-copy receive
- **THEN** the build fails with an error naming the kernel feature

#### Scenario: Card not splitting headers
- **WHEN** a backend with a receive area is built on an interface whose queue does not split headers from payload
- **THEN** the build fails with an error naming the card setting

#### Scenario: With TLS
- **WHEN** a connection is opened in the mode with TLS
- **THEN** opening it fails

#### Scenario: Fewer frame buffers than connections
- **WHEN** a connection set is planned with pages for the mode and fewer frame buffers than it has connections
- **THEN** planning fails with an error naming the frame count

### Requirement: Only whole pages held, under a bound the kernel keeps
The mode SHALL hold past a turn only whole pages that each hold one whole
block of one frame lent in pages, never more of them than the page budget
sized at build, and SHALL arm a connection's receive only for the rest of
its header or of its frame, for which it already has a place, so that no
received byte waits for memory.

#### Scenario: Every connection part-way through a large frame
- **WHEN** every connection of a set with the fewest frame buffers planning allows is part-way through a frame, with the smallest page budget, and their senders go on sending
- **THEN** every frame is delivered, and no connection waits for ever

#### Scenario: A connection that is not pulled
- **WHEN** the caller does not pull one connection while others receive
- **THEN** that connection holds one frame and reads no further, and the others go on

#### Scenario: The page budget used up
- **WHEN** a frame lines up and the page budget has no room for it
- **THEN** it is delivered in frame memory, and nothing waits

### Requirement: Frames in pages held and returned once
A frame lent in pages SHALL stay readable until the caller's hold and the
hold of every file write that took one are gone. The caller's hold SHALL
be given up once, by `release` or by `hold`, a stale or second release
doing nothing whatever else still holds the frame. Its pages SHALL be
returned to the kernel only then, each piece exactly once, and never
while a file write that holds the frame has not completed.

#### Scenario: Release returns the pages
- **WHEN** every frame is released after a run of frames in and out of step
- **THEN** no piece is out, the page budget is whole, and the set is idle

#### Scenario: A copy of a frame released twice
- **WHEN** a frame in pages is released and a copy of it is released again
- **THEN** the second release does nothing, and no other frame's pages are returned

#### Scenario: Released twice under a write
- **WHEN** a frame in pages is under a file write that holds it, and the caller releases it and then releases a copy of it again
- **THEN** its pages stay with the write until the write completes, and the file holds the frame's bytes

#### Scenario: Held for relaying while under a write
- **WHEN** a frame in pages is under a file write that holds it, and the caller calls `hold` on it to send it on
- **THEN** the caller gets the frame in frame memory, its pages stay with the write until the write completes, and its length is counted against the connection's held bytes once

#### Scenario: A connection set given up with frames lent
- **WHEN** a connection set is told to give back every piece while frames in pages are still held
- **THEN** no piece is out afterwards and another set on the same backend receives into the whole area

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
- **THEN** it is armed again for the rest of its credit, never with a credit of zero, and the frame is delivered whole

#### Scenario: A receive left silent by a full completion ring
- **WHEN** the completion ring fills part-way through a frame that lies whole in the socket, and no further packet comes
- **THEN** the frame is delivered whole all the same

#### Scenario: The kernel's free pages never come back
- **WHEN** a connection's receive ends for want of the kernel's free pages sixteen times in a row with nothing delivered
- **THEN** the connection ends with an error, and the other connections go on

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
returned twice, on the wrong piece of a page returned, on a piece read
after its return or returned under a file write, and on a receive armed
with a credit of zero, and the model test and the transport conformance
suite SHALL run in the mode.

#### Scenario: Same seed, same trace
- **WHEN** the model test runs twice in the mode with one seed
- **THEN** both runs produce the same trace hash

#### Scenario: A read after return is seen
- **WHEN** a fault is put into the set that reads a piece after returning it
- **THEN** the model test fails

#### Scenario: A return under a write is seen
- **WHEN** a fault is put into the set that returns a frame's pieces while a file write still holds the frame
- **THEN** the model test fails on the file's bytes

### Requirement: A frame on a packet of its own
A connection SHALL offer to send every frame so that it starts a packet:
header and body as one send, and not before every byte of the frame
before it has left the socket. The option SHALL change nothing a receiver
in another mode sees but the cut of the stream.

#### Scenario: Frames back to back with the option
- **WHEN** a sender with the option sends frames of unequal lengths to a receiver in the mode, with packets that carry whole pages and no loss
- **THEN** the frames long enough to be kept are lent in pages

#### Scenario: Frames back to back without it
- **WHEN** the same frames are sent without the option
- **THEN** every frame is delivered, moved or lent, and its bytes are the bytes sent

### Requirement: Measured against the copying modes
The mode's CPU cost per byte SHALL be measured on a card against ring and
direct mode, counting the cores that handle the queue's packets, for
frames written to a log as they lie, frames read in place and frames that
do not line up, with the share of frames lent in pages, and the results
SHALL be recorded in `bench/results/` with the card settings used.

#### Scenario: Result on record
- **WHEN** the change's tasks are marked done
- **THEN** `bench/results/` holds the comparison and `docs/tcp.md` the settings and the hosts it ran on
