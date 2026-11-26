## Context
Measured on two DGX Sparks (kernel 7.0, ConnectX-7 `mlx5_core`, one
200 Gb/s port seen as two interfaces of about 110 Gb/s each), milestone 30:

| | GB/s a core | CPU-seconds a GB |
|---|---|---|
| Receive, ring, kernel TCP | 5.4 to 7.3 | 0.13 to 0.16 |
| Send, zero-copy, 1 MiB segments | not the limit | 0.024 to 0.030 |
| Send, copied | 5.3 to 6.1 | 0.083 to 0.186 |

Kernel TCP fills the link (198 Gb/s) with eight receiving cores at 77 %.

What `zcrx` is, from the kernel's documentation
(`Documentation/networking/iou-zcrx.rst`): a ring registers one receive
queue of one interface with a memory area and a refill ring; a multishot
`IORING_OP_RECV_ZC` on a socket then completes with offsets into the
area; the program returns each buffer through the refill ring. It needs
the ring set up with single issuer, deferred task work (both of which
longshore already uses) and 32-byte or mixed completions; a card that
splits headers from payload; and flow steering plus RSS set by the
operator so that only the wanted flows reach the queue. The area is cut
into chunks of the page size unless a larger length is asked and granted.

## Goals / Non-Goals
- Goals: receive without the kernel's copy on the ring backend; the same
  `Net` API with one addition; the same contract, tests and refusals as
  the other receive modes; numbers that decide the AF_XDP change.
- Non-Goals: a user-space TCP stack; configuring the card; `zcrx` on the
  portable backend; a second memory pool for files.

## Decisions
- **A receive mode, not a transport.** The kernel still does TCP, so
  everything above receive (states, events, close, TLS, reconnect) is the
  kernel transport's. `Rx::ZeroCopy` is chosen per listener or connection
  like the other modes.
- **Chunks take the place of ring buffers.** Ring mode already lends a
  frame that lies in one buffer and assembles one that does not. Here a
  frame in one chunk is lent the same way; the difference is the frame
  that spans chunks.
- **A spanning frame is lent as pieces.** With chunks of 4 KiB every frame
  larger than that spans, so assembling would copy everything and the
  mode would win nothing. `Net::pieces(&frame)` yields the frame's byte
  runs in order; the frame holds its chunks until `release`. `hold` moves
  it once into block-aligned frame memory for a caller that needs one run.
  - Alternatives considered: larger chunks only (`rx_buf_len`): needs
    contiguous memory and driver support the documentation calls
    optional, and a frame still starts wherever the stream puts it, so a
    block-aligned write needs the move anyway. Kept as a setting to
    measure, not as the design.
- **The log path keeps its one move.** A direct-I/O write needs a
  block-aligned run; payload arrives behind the stream's own offsets.
  Receive, `hold`, write, send to replicas is one move, as with `Direct`
  today. What zero-copy receive removes is the copy for frames handed on
  unread (forwarded, replicated without a log write); a frame the caller
  reads saves less (milestone 31), and a small request in ring mode was
  never the cost.
- **One queue a ring, one ring a thread.** A registration binds a ring to
  one receive queue. A thread's listener must therefore be reached only
  through that queue: the operator steers the listener's port (or the
  thread's address) to it. With the Spark's two interfaces a thread
  serves one of them.
- **Accounting as one structure**, by piece, not by chunk (see "What the
  kernel does" and the tables below); checked after every step of the
  model test.
- **Exhaustion is answered before the area is empty.** An empty area
  costs seconds (below), so the mode never relies on it: the pieces a
  backend may have out are bounded at build, receiving stops at that
  bound, and the area is sized so that the bound is reached first.
- **TLS.** Records are opened from pieces into frame memory, as from ring
  buffers today; the mode saves the kernel's copy, not the decryption's.
- **Zero-copy sends.** The measured case fits sixteen sends in flight a
  connection and a notification that comes milliseconds after the send.
  Task 4.1 traces where it waits before anything is changed; the
  candidates are a larger default segment for bulk senders and counting a
  zero-copy send as done for the queue when its first completion comes,
  holding only its memory until the notification.

## Review of the contract: not passed
The sections from "What the kernel does" to "The simulator's model" were
reviewed against the kernel's source and the existing code before any
code was written, and do not stand as written. No code follows from them
until they are redone. What the review found, in the order that matters:

1. **The stop rule deadlocks.** A piece that is delivered but not yet part
   of a whole frame can only leave by becoming one. Thirty-two connections
   each part-way through a 1 MiB frame fill any table; every receive is
   then stopped and nothing can ever be released. Ring mode avoids this
   by copying a spanning frame into frame memory at once and by moving
   unpulled input out at every `poll`; this design carried neither over.
2. **The bound cannot be held where it was put.** The kernel delivers
   until the socket is empty or the completion ring is full; it knows
   nothing of `pieces_max`. Completions already delivered when the table
   fills cannot be refused. And ring mode does not "cancel its receives
   when the ring runs dry": the kernel ends them there.
3. **One connection can take everything.** Nothing bounds one
   connection's pieces: a connection the caller does not pull, or a frame
   lent to a peer that reads slowly, stops every connection of the mode.
4. **A piece has several holders at once.** A frame can be with the
   caller, in sends to several peers and under a file write together;
   the table gave a piece one place. `Frame` is `Copy` and a ring-lent
   frame needs no `release` today, so a program that is correct in ring
   mode would leak or return twice here.
5. **Sending a frame in pieces does not fit.** A 1 MiB frame is about 290
   pieces; a connection queues sixteen sends and the backend has no
   vectored send.
6. **TLS copies anyway.** A record spans four or five pieces and is
   gathered before it is opened.
7. **The sizing rule is not computable**: socket buffers are charged by
   the kernel's own measure, not in chunks; backlog, accept queues and
   other processes' flows on the queue hold chunks unseen; no call sets a
   socket's receive buffer today.
8. **Claims about the kernel that need redoing**: an overrun refill ring
   is read twice (double returns), not merely lost; the copy path draws
   only from the area's free list and can fail with `ENOMEM` on an area
   that is not empty; a flow from another ring's queue fails with
   `EFAULT`; what a full completion ring does (K5) and whether an
   ordinary receive works on such a socket (it would break the TLS
   handshake and direct receive) are not established; the flush call is
   not in 6.15, so the floor is nearer 7.0.
9. **The stall run shows less than was claimed**: one run, an area empty
   for a second by construction, order and content of the stream not
   checked, and its own returns in the last phases not bounded.

## What the mode can be worth, after milestone 31 and this review
With the two modes that exist, a frame that is logged costs one copy (the
kernel's, straight into block-aligned memory), and so does a frame that
is forwarded (received direct, sent zero-copy). Zero-copy receive can
only improve on that for:

- a frame read in place and not kept: a quarter less on the receiving
  thread, a tenth counting the queue's core; and
- a frame forwarded with no copy at all, which needs pieces held across
  turns, counted holds and a send from the area: findings 1 to 5.

## Decided (2026-11-26), from UltraGraphDB's protocol and three more runs
The database's protocol (`../ultragraphdb/specification/binary_protocol/
PROTOCOL.md`) settles what the mode is for:

- **Its frames**: a 256-byte header and a body, 256 bytes to 1 MiB, back
  to back; every body byte under a checksum the receiver must check, a
  leaf a 4 KiB block of the frame.
- **Replication (its stage 8)** carries the log's Round frames as they
  are: there the bytes on the wire are the bytes of the log. This is the
  traffic the mode is built for, by the owner's decision.
- **A client's bulk load** is not logged as received: its body goes into
  a Round frame at a block boundary, 256 bytes off from where it lay in
  its own frame. Zero-copy receive cannot help that; receiving the frame
  so that its body lies at a block can. That comes first, by the owner's
  decision, and is group 6.

And the runs (milestone 31) settle what is possible:

- With a packet's payload a whole number of pages (MTU 8244), every piece
  is a whole page.
- The kernel accepts a vectored direct-I/O write from the area's pages;
  card to disk without a copy costs the thread 0.066 CPU-seconds a GB
  against 0.154.
- A frame lies in whole pages when it starts a packet; one short packet
  inside a frame puts the rest of that frame out of step.
- The body at a block, in direct mode, saves 6 % of the thread.

So the mode is the full one, shaped by these:

- **It is for frames that are written as they lie.** A frame that starts
  a packet and stays in step is lent as its pages and held until its
  write is durable; any other frame is moved once into frame memory, and
  is then what direct mode gives today. The gain is opportunistic by
  frame, never a loss against direct mode.
- **Only whole pages are ever held.** A piece that is not a whole page of
  a frame in step is moved on arrival and returned in the same turn.
  That removes the one-byte piece that pins a page.
- **The kernel holds the bound.** A zero-copy receive is armed with a byte
  credit and ends when it is used (`io_recvzc`, `sqe->len`); a connection
  gets credit only for what it may hold, and more when frames are
  released. This takes the place of the stop rule that deadlocked.
- **Plain TCP only, and no other kind of receive on such a connection.**
  TLS and Unix sockets are refused with the mode when a connection is
  opened or a listener made.
- **A write from pages is a new operation** of the backend: a vectored
  file write over a list of area pages.
- **The floor is Linux 7.0**, the kernel it was read from and run on.

The contract below is the first one, which did not pass. It is rewritten
to these decisions, with the code of `rx.rs`, `pull.rs`, `spill.rs` and
`frames.rs` read first, and reviewed again before any code of the mode.

## What the kernel does (Linux 7.0, `io_uring/zcrx.c`, and a run)
Read from the source of the kernel the Sparks run, and checked by the
`stall` run of `bench/spikes/zcrx/zcrecv.c` (milestone 31).

- **K1 A reference a completion, not a chunk.** Every completion takes one
  user reference on the chunk its bytes lie in, and one entry in the
  refill ring gives one back. A chunk can back several completions (the
  run held 72,750 completions on an area of 65,536 chunks). An entry for
  a chunk with no reference is ignored; one entry too many for a chunk
  that still has a reference takes away another completion's.
- **K2 The refill ring is bounded and read lazily.** At most 32,768
  entries; the kernel takes up to 64 when the card's pool wants pages and
  reads no more than the ring's size, so entries written past a full ring
  are lost. `IORING_REGISTER_ZCRX_CTRL` with `ZCRX_CTRL_FLUSH_RQ` makes it
  take them all, in one system call. In the runs the ring never held more
  than 1,219.
- **K3 An empty area drops packets.** The pool hands the card nothing, the
  card drops (365 `rx_out_of_buffer`, 64 `buff_alloc_err` in the run),
  TCP sends again (377 segments). The stream stayed whole and in order,
  but after every chunk was returned it took 5.4 seconds to carry data
  again, and every flow of the queue waits with it. Bytes waiting in a
  socket's receive queue lie in chunks too.
- **K4 Copying behind the same face.** Payload that did not arrive in the
  area (a flow on another queue, bytes in a packet's header part) is
  copied by the kernel into a free chunk and completed like any other. A
  program cannot tell; with no free chunk the receive ends with `ENOMEM`.
- **K5 A full completion ring** makes the kernel try the receive again
  later; nothing is dropped.
- **K6 TCP only** (`EPROTONOSUPPORT` otherwise), `CAP_NET_ADMIN` to
  register (`EPERM`), deferred task work and 32-byte or mixed completions
  (`EINVAL`), header and data split on the card (`EINVAL`), chunk lengths
  above a page refused by this driver (`EOPNOTSUPP`).
- **K7 Dropping the ring** takes every chunk back, whoever holds it.

## A piece and its chunk
A **piece** is what one completion delivers: a run of bytes in one chunk.
The backend keeps a table of pieces out, sized at build (`pieces_max`).

| Where a piece is | Who may read it | Leaves by |
|---|---|---|
| Delivered: in a connection's input, not yet a frame | the set | becoming part of a frame; the connection closing (returned) |
| Lent: part of a frame the caller holds | the caller, through `pieces` or `bytes` | `release` (returned); `hold` (moved once, then returned) |
| Sending: part of a frame being sent on | the kernel | the send's last completion (returned) |
| Returned, waiting: in the backend's own queue of returns | nobody | room in the refill ring |
| Returned: in the refill ring | nobody | the kernel taking it |

A chunk is with the kernel while no piece of it is in the first three
rows. The simulator fills a chunk with a marker when its last piece is
returned, so a read after return is seen.

## A queue's registration
| State | Entered | Left |
|---|---|---|
| None | a backend built without the mode | never |
| Registered | at build; refusals are build errors (K6) | the backend dropped (K7) |
| Lost | the operator reconfigures or takes down the interface | never: armed receives of the mode end with an error and their connections close with `Closed { why: Io }`; nothing registers again at run time |

Whether the kernel tells a ring that its registration was taken away, and
how, is established in task 3.1 before `Lost` is implemented.

## Invariants
Checked after every step of the model test, on the simulator's area.

- **Z1 Once.** Every piece delivered is returned exactly once. (K1: a
  second return would free memory another piece is read through.)
- **Z2 Not after.** No byte of a piece is read after its return.
- **Z3 In order.** A connection's pieces are consumed in the order
  delivered; a frame's pieces are consecutive pieces of one connection.
- **Z4 Bounded out.** Pieces in the first three rows never exceed
  `pieces_max`.
- **Z5 Bounded ring.** The refill ring never holds more entries than it
  has; what does not fit waits in the backend's queue, which has
  `pieces_max` places and so cannot be full.
- **Z6 Nothing kept at rest.** With every frame released and every
  connection closed, no piece is out and both queues of returns drain.
- **Z7 After close.** Frames of a closed connection stay readable until
  released; their pieces are returned then.
- **Z8 One move.** `hold` copies a frame's bytes once into frame memory
  and returns its pieces; nothing else in the mode copies payload.
- **Z9 At build.** The area, the refill ring, the piece table and the
  queue of returns are allocated at build; nothing afterwards.
- **Z10 The same events.** The mode changes no state, event or their
  order against ring mode for the same bytes.

## What runs out, and what then
| Runs out | What happens | Ends when |
|---|---|---|
| Places in the piece table (`pieces_max`) | The set cancels its armed receives of the mode, as it does when the buffer ring runs dry; input waits in the kernel and TCP's window closes. No event; `Usage` counts it. | Frames are released: the receives are armed again |
| Room in the refill ring | Returns wait in the backend's queue; the next `tick` writes what fits and asks the kernel to take them (K2), one system call | The same tick |
| Frame memory for `hold` or a TLS record | As in ring mode today: the frame stays in its pieces, the call says `Full` | Frame memory is released |
| Room in the completion ring | The kernel delivers later (K5) | The next `tick` |
| Chunks in the area | Packets dropped, sent again, seconds lost (K3). Not a state the set handles: it is kept from happening by sizing | See the sizing rule |

**The sizing rule.** `Config::plan` refuses a plan in which the area can
empty before the piece table is full: chunks at least `pieces_max`, plus
every connection's socket receive buffer (which the transport sets in
this mode, so that it is known), plus the card's receive ring and its
pool's cache. A chunk can back more than one piece, so `pieces_max`
chunks is the worst case, not the usual one.

## The simulator's model of the mode
`SimIo` gets an area of chunks and delivers an armed receive of the mode
as pieces: seeded lengths from one byte to a chunk, seeded sharing of a
chunk by consecutive pieces, up to twenty pieces a turn, the bytes copied
in behind the same face now and then (K4). It keeps the reference count
of K1, marks returned chunks (Z2), refuses a second return of a piece and
a return of one never delivered (Z1), holds a refill ring of seeded,
small size that it reads lazily (K2, Z5), and delivers nothing while no
chunk is free, for a seeded time longer than the usual delays (K3).

## Risks / Trade-offs
- The driver may not support it on the Sparks → the spike of group 1 is
  first and is allowed to end the change.
- A queue given to one ring is taken from the kernel's RSS → documented;
  the refusal names an unsteered queue where it can be detected.
- `CAP_NET_ADMIN` is needed to register → refusal names it.
- 32-byte completions double the completion ring's memory → only when the
  mode is asked for.
- The registration answers a missing header and data split with a bare
  `EINVAL`, as it does other faults → the builder asks the card for the
  setting first (ethtool's ring parameters over netlink) and names it;
  what cannot be told apart is reported as the registration's own error
  with the settings it needs listed.
- The kernel's packet handling costs more a byte in this mode (0.035
  against 0.014 CPU-seconds a GB on the queue's core) → results count
  that core, not the thread alone.
- The saving may be small where the consumer touches every byte anyway
  (the copy's cache misses move to the consumer) → measured per case
  before any default changes.

## Migration Plan
None: an added mode, off unless chosen. Rollback is not choosing it.

## Spike result (milestone 31): go
`mlx5_core` in 7.0 takes the registration once `ethtool -G IFACE
tcp-data-split on` is set (refused with `EINVAL` before). CPU-seconds a
GB, receiving thread and queue's core together: 0.07 without the copy
against 0.14 with it when payload is not read; 0.155 against 0.177 when
every byte is read. On the thread alone: 0.035 against 0.128, and 0.117
against 0.159. Completions carry one page at most, and larger chunks are
refused by the driver.

## Open Questions
- Can memory of the area also be registered as fixed buffers, so that a
  frame is forwarded from where it lies with a zero-copy send? Task 3.5
  answers it; if not, the frame's pieces are sent as they lie, by the
  kernel's copying send.
- Mixed completions (`IORING_SETUP_CQE_MIXED`) instead of 32-byte ones:
  from which kernel, and is it worth a second floor?
