# The receive modes of `longshore_tcp`, and which one UltraGraphDB uses where

`longshore_tcp` receives in one of three modes, chosen per listener or
connection with `ConnOpts::with_rx`. Two exist (`Rx::Ring`, `Rx::Direct`);
the third (`Rx::ZeroCopy`) is proposed in `add-tcp-zero-copy-receive` and
only measured by a test program so far. This note says what each does,
what it costs, and which fits which traffic of a database node.

The numbers are from two DGX Sparks over 200 GbE
(`bench/results/m30-tcp-dgx-spark-pair-200gbe-aarch64.md`,
`m31-tcp-zero-copy-receive-spike-dgx-spark.md`): one to three runs each,
on machines that do other work. Read them as "about", and as true of that
card and that disk. What `Rx::ZeroCopy` would do is measured with a test
program (`bench/spikes/zcrx/zcrecv.c`), not with `longshore_tcp`.

## What costs CPU when receiving

Every received byte is copied once by the kernel, from its own memory
into the program's. That copy is nearly all a receiving thread does: it
fills a core at 43 to 58 Gb/s, while a thread that sends the same bytes
without copying spends a sixth of that. The three modes differ in where
that one copy lands, and whether it happens at all.

## The three modes

| | `Rx::Ring` | `Rx::Direct` | `Rx::ZeroCopy` (proposed) |
|---|---|---|---|
| Where payload arrives | A ring of buffers shared by all connections | The frame's own block-aligned buffer | Pages of an area the card writes into |
| Kernel copies payload | Yes | Yes | No |
| A frame is lent | In place if it lies in one ring buffer; put together in frame memory if not | In place, always | In place if in one page; as its pieces, in order, if not |
| Ready for a direct-I/O write | After `hold` (one move) or after being put together | As it is; with `with_body_at_block`, its body at a block | As its pages lie, when the frame starts a packet and packets carry whole pages; after one move otherwise |
| Operations a frame | Fewest: one armed receive serves everything | Two receives (header, then body) | One armed receive; one completion a page |
| Receiving thread's CPU-seconds a GB | 0.13 to 0.16 | 0.13 to 0.16 | 0.035 if the bytes are not read; 0.12 if they are, against 0.16 for a copy that is then read |
| Needs | Nothing | Nothing | Linux 7.0, a card that splits headers from payload, a receive queue set aside by the operator, root, plain TCP; for whole pages, an MTU of 8,244 |

- **Ring** is the mode for many connections with small frames: a request
  that fits one ring buffer costs no move and the fewest system calls.
  Measured over the wire at 324 bytes: 310,000 requests a second on 256
  connections.
- **Direct** is the mode for large frames and for frames that are logged:
  the body lands at the start of a block-aligned buffer and goes to the
  log as it lies, with no move of our own. It pays two receives a frame,
  which small frames feel.
- **Zero-copy** removes the kernel's copy. It pays off most for payload
  that is handed on without being read: half the CPU, counting the core
  that handles the queue's packets as well (0.07 against 0.14
  CPU-seconds a GB), and one thread carries 66 to 87 Gb/s. Payload that
  is read on the way gains less: a quarter of the thread's cost, a tenth
  counting both cores, because reading it for the first time is dearer
  than reading what a copy has just brought into the cache. Payload that
  is logged can go to the disk as its pages lie, with no copy at all,
  when two things hold: packets carry a whole number of pages (an MTU of
  8,244 gives 8,192 bytes of payload, two pages), and the frame starts a
  packet. A frame that does not line up is moved once, as in direct mode.
  On the Sparks' card a page is 4 KiB and nothing larger is granted.

## What a database node receives

What follows takes UltraGraphDB as it is described in the issue this work
answers (thread-per-core; each thread owns its connections, its log
written with direct I/O, and its replicas) and in the `path` benchmark
modelled on it (a block received, logged, sent to three replicas and
answered). Where the database differs, the row changes with it.

| Traffic | What happens to the payload | Mode | Why |
|---|---|---|---|
| Queries and small writes from clients | Parsed and answered; a few hundred bytes | `Ring` | Fits a ring buffer; lent in place; fewest operations |
| Bulk load from clients (Ingest) | Checked; its body goes into a Round frame at a block boundary, 256 bytes off from where it lay in its own frame | `Direct` with `with_body_at_block(4096)` | The body lands at a block and is logged from where it lies; zero-copy receive cannot give that shift |
| Round frames from the leader, on a replica (stage 8) | Checked, then logged as received: the bytes on the wire are the bytes of the log | `ZeroCopy`, once built; `Direct` until then | Written as the pages lie when the frame lines up, moved once when not |
| Payload relayed to another node unread | Sent on | `ZeroCopy`, once built | Nothing reads it, so the copy is pure cost |
| Payload the node checks on arrival (a checksum over the body) | Read in full, then logged | `Direct` | Ready for the log as it lies; zero-copy would save a little on the read and pay the move for the log |
| Large results streamed from another node into a query | Read by the query, not logged | `ZeroCopy`, once built; `Direct` until then | Read in place: a quarter less on the receiving thread |

UltraGraphDB's protocol puts every body byte under a checksum the
receiver must check, so its payload is always read once: the figures for
"read" apply to it, not those for "not read".

## From the card to the disk, end to end

One receiving thread on one Spark, one sender on the other, four
connections of 1 MiB frames, the receiver writing to its NVMe disk with
direct I/O. The receiver is the test program; it writes one 1 MiB write
after another, waiting for each.

| What the receiver does | GB/s | Thread, CPU-seconds a GB |
|---|---|---|
| Zero-copy receive, pages written as they lie (MTU 8244) | 3.8 | 0.066 |
| Kernel's copy, buffer written (MTU 8244) | 2.7 | 0.154 |
| Kernel's copy, every byte read, written as received | 2.3 to 2.4 | 0.19 to 0.20 |
| The same, body moved to a block first (an Ingest today) | 2.2 | 0.20 to 0.21 |
| The same, body received at a block (`with_body_at_block`) | 2.4 | 0.19 |

And the parts on their own:

| Part | What it carries |
|---|---|
| The link, both interfaces of the port, kernel TCP | 24.7 GB/s (198 Gb/s) with eight receiving cores |
| One interface of the port | about 13.8 GB/s (110 Gb/s) |
| One receiving thread, kernel's copy | 5.4 to 7.3 GB/s |
| One receiving thread, zero-copy, nothing written | 8.3 to 10.9 GB/s |
| The disk, one direct-I/O write at a time | 3.3 to 3.5 GB/s |
| The disk, eight or more writes in flight | 13.1 GB/s for 18 seconds (about 236 GB), then about 3 GB/s |
| The disk, one write at a time, after that | 1.3 to 1.5 GB/s |

What these say together:

- **The end-to-end rows above are held by one write at a time**, not by
  the card, the receiving thread or the disk's capacity: one writer gets
  3.3 to 3.5 GB/s from this disk by itself. They compare CPU a byte
  fairly; they do not show what the path can carry.
- **With several writes in flight the disk matches one interface** for as
  long as its fast cache lasts: 13 GB/s against 13.8. A receiving thread
  then becomes the limit, at 5 to 7 GB/s with the kernel's copy and 8 to
  11 without it. That combination has not been run: the test program
  does not keep writes in flight, and `longshore_tcp` has no zero-copy
  receive yet.
- **Sustained, this disk is the limit; in operation that is rare.** After
  about 236 GB at full rate it takes about 3 GB/s, less than one
  receiving thread delivers either way. Only the first import of a large
  data set writes that much without a pause. A database in operation
  writes in bursts far below 236 GB, and for those the burst rate is the
  one that counts: 13 GB/s, where the receiving thread is the limit and
  zero-copy receive carries more as well as costing less. One thing is
  not measured: how fast the disk's fast cache comes back. A run twenty
  seconds after the cache was filled still got 1.3 to 2.9 GB/s, so
  bursts that follow a very large one closely are slower for a while.
- **CPU a byte is where the modes differ**: 0.066 against 0.154
  CPU-seconds a GB from the card to the disk when the payload is not
  read. With the checksum's read on top the difference was not measured
  end to end; for payload that is read and not written it is 0.117
  against 0.159.

## How many cores, which interface

- **Receiving scales with cores.** One thread a core, each with its own
  connections, as the database is built: two receiving cores carry twice
  what one does. Kernel TCP fills the Sparks' 200 Gb/s link with four to
  eight receiving cores of twenty.
- **A Spark's port is two interfaces of about 110 Gb/s each.** The full
  link needs both, each with an address of its own and threads spread
  over them. This is how the Spark attaches its card, not a property of
  the card: a server with the card in a full-width slot shows one
  interface.
- **Zero-copy receive binds a thread to one receive queue** of one
  interface; the operator steers the listener's port to it. The other
  modes need no such setting.
- **Whole pages need the packet size set on both ends** (MTU 8,244 on the
  Sparks' card), and a sender that starts each frame on a packet of its
  own. Neither is needed for correctness: a frame that does not line up
  costs one move.

## Sending, for completeness

Sending from registered memory without a copy costs 0.024 to
0.030 CPU-seconds a GB. One fault is open: zero-copy sends of one 64 KiB
segment each are slow over a card (about 1 GB/s on four connections
against 6 GB/s copied). Until it is fixed in `add-tcp-zero-copy-receive`,
a bulk sender should use segments of 1 MiB (`Sizes::with_segments`), with
which the fault does not show.
