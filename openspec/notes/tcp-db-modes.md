# The receive modes of `longshore_tcp`, and which one UltraGraphDB uses where

`longshore_tcp` receives in one of three modes, chosen per listener or
connection with `ConnOpts::with_rx`. Two exist (`Rx::Ring`, `Rx::Direct`);
the third (`Rx::ZeroCopy`) is proposed in `add-tcp-zero-copy-receive` and
only measured by a test program so far. This note says what each does,
what it costs, and which fits which traffic of a database node.

The numbers are from two DGX Sparks over 200 GbE
(`bench/results/m30-tcp-dgx-spark-pair-200gbe-aarch64.md`,
`m31-tcp-zero-copy-receive-spike-dgx-spark.md`): one run each, on machines
that do other work. Read them as "about", and as true of that card.

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
| Ready for a direct-I/O write | After `hold` (one move) or after being put together | As it is | After `hold` (one move) |
| Operations a frame | Fewest: one armed receive serves everything | Two receives (header, then body) | One armed receive; one completion a page |
| Receiving thread's CPU-seconds a GB | 0.13 to 0.16 | 0.13 to 0.16 | 0.035 if the bytes are not read; 0.12 if they are, against 0.16 for a copy that is then read |
| Needs | Nothing | Nothing | Linux 6.15, a card that splits headers from payload, a receive queue set aside by the operator, root |

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
  is logged still needs one move into block-aligned memory, because it
  arrives in pages at whatever offset the stream puts it. On the Sparks'
  card a page is 4 KiB and nothing larger is granted.

## What a database node receives

What follows takes UltraGraphDB as it is described in the issue this work
answers (thread-per-core; each thread owns its connections, its log
written with direct I/O, and its replicas) and in the `path` benchmark
modelled on it (a block received, logged, sent to three replicas and
answered). Where the database differs, the row changes with it.

| Traffic | What happens to the payload | Mode | Why |
|---|---|---|---|
| Queries and small writes from clients | Parsed and answered; a few hundred bytes | `Ring` | Fits a ring buffer; lent in place; fewest operations |
| Bulk load from clients | Logged, then replicated | `Direct` | Lands block-aligned, written and sent from where it lies with no move |
| Blocks from the leader, on a replica | Logged | `Direct` | Same |
| Payload relayed to another node unread | Sent on | `ZeroCopy`, once built | Nothing reads it, so the copy is pure cost |
| Payload the node checks on arrival (a checksum over the body) | Read in full, then logged | `Direct` | Ready for the log as it lies; zero-copy would save a little on the read and pay the move for the log |
| Large results streamed from another node into a query | Read by the query, not logged | `ZeroCopy`, once built; `Direct` until then | Read in place: a quarter less on the receiving thread |

So for the database as modelled, the two existing modes already cover the
logged paths, and they do it with one copy a byte, the kernel's.
Zero-copy receive wins most where payload passes through a node
untouched, and something where large payload is read but not logged. How
much of UltraGraphDB's traffic is of those two kinds decides what the
mode is worth to it; that is the question to answer before group 3 of
the change is built for the database's sake.

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

## Sending, for completeness

Sending from registered memory without a copy costs 0.024 to
0.030 CPU-seconds a GB. One fault is open: zero-copy sends of one 64 KiB
segment each are slow over a card (about 1 GB/s on four connections
against 6 GB/s copied). Until it is fixed in `add-tcp-zero-copy-receive`,
a bulk sender should use segments of 1 MiB (`Sizes::with_segments`), with
which the fault does not show.
