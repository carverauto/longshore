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
- Goals: frames that arrive in whole pages lent and written to a log
  without a copy, on the ring backend; never worse than direct mode;
  the same contract, tests and refusals as the other receive modes;
  numbers that decide the AF_XDP change.
- Non-Goals: a user-space TCP stack; configuring the card; the mode on
  the portable backend, with TLS or on Unix sockets; sending a frame from
  the pages it arrived in.

## Decisions
What the mode is and how it works is in "Decided" and "The contract"
below; they replace the first decisions, of which these stand:

- **A receive mode, not a transport.** The kernel still does TCP, so
  states, events, close and reconnect are the kernel transport's.
- **One queue a ring, one ring a thread.** A registration binds a ring to
  one receive queue. A thread's connections must be reached only through
  that queue; the operator steers them to it. With the Spark's two
  interfaces a thread serves one of them.
- **Larger pages are not the design.** The driver refuses them
  (milestone 31), and a frame would still start wherever the stream puts
  it.
- **Zero-copy sends** (group 4). The measured case fits sixteen sends in
  flight a connection and a notification that comes milliseconds after
  the send. Task 4.1 traces where it waits before anything is changed;
  the candidates are a larger default segment for bulk senders and
  counting a zero-copy send as done for the queue when its first
  completion comes, holding only its memory until the notification.

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

The contract that follows is written to these decisions and is reviewed
again before any code of the mode.

## The contract
Written to "Decided" above after reading `net/rx.rs`, `pull.rs`,
`spill.rs`, `frames.rs`, `tx.rs`, `state.rs`, `transport.rs`, the kernel's
`io_uring/zcrx.c` and `io_uring/net.c` of v7.0, and UltraGraphDB's
write-ahead-log spec ("a round SHALL be written as its frame, header and
body, at a 4,096-byte boundary, padded to whole blocks, in one write").
It replaces the contract that did not pass. Names marked *new* do not
exist yet; every other name is the code's.

### C1 Scope and refusals
The mode is `Rx::ZeroCopy` (*new*), a third receive mode of a connection
or a listener. It is direct mode with one difference: where a frame's
blocks arrive lying in whole pages, the frame is lent as those pages
instead of being moved into frame memory.

| Asked for | Answer | Where |
|---|---|---|
| A backend with a receive area on the portable backend, on a kernel below 7.0, without `CAP_NET_ADMIN`, on a queue without header and data split, or with an area the memory-lock limit does not allow | `BuildError` naming which (C2, K8) | `Builder::build` |
| `Rx::ZeroCopy` on a `Net` whose backend has no receive area, or whose `Sizes` have no pages for it | `NetError::Framing`, as for any option the set was not sized for (`fits`) | `connect`, `listen` |
| `Rx::ZeroCopy` with TLS | `NetError::NoTls` | `connect_tls`, `listen` with `with_tls` |
| `Rx::ZeroCopy` on a Unix-domain address | `NetError::Framing` | `connect`, `listen` |
| `Rx::ZeroCopy` with `Framing::Bytes` | `NetError::Framing`: the mode is of frames | `connect`, `listen` |
| `Rx::ZeroCopy` on a listener with `with_reuse_port` | `NetError::Framing`: a port shared between rings lets the kernel hand a connection to a ring whose queue its packets do not reach (K5) | `listen` |

No receive other than the zero-copy receive is ever issued on a
connection of the mode: there is no handshake, and the single receives of
`read_direct` and `read_kept` are not used. Whether an ordinary receive on
such a socket works is therefore not needed, and is not known.

What the program cannot check, and the operator must provide: that the
connection's packets reach the registered queue (a flow rule by the
listener's port; for a connection that is dialled, by the peer's address
and port), and that no other flow does. A flow that reaches another queue
is served by the kernel's copy (K4) and behaves as direct mode at more
cost; a flow from a queue registered to another ring ends the connection
(K5).

### C2 What the kernel does (Linux 7.0)
From `io_uring/zcrx.c` and `io_uring/net.c`; "run" means milestone 31.
Where the source does not show something, it says so and names the task
that settles it.

- **K1 A reference a completion.** Every data completion takes one
  reference on the page its bytes lie in (`io_zcrx_get_niov_uref`); one
  entry in the refill ring gives one back. A page can back several
  completions (the run: 72,750 held on 65,536 pages; a first page of
  1,280 then 2,816 bytes). The kernel reads only the page of an entry, not
  its offset or length; an entry for a page with no reference is skipped;
  one entry too many for a page takes away another completion's reference.
- **K2 The credit.** `IORING_OP_RECV_ZC` is multishot only, and its
  `sqe->len` is a count of bytes: the request delivers at most that many
  and then completes with result 0 and no more to follow (`io_recvzc`:
  `len && zc->len == 0`). End of stream completes the same way, with
  result 0. The program tells them apart by its own count: a receive that
  ends with 0 after delivering exactly its credit ended for the credit;
  one that ends with 0 after less saw the end of the stream.
- **K3 What ends a receive.** Any negative result but `-EAGAIN` completes
  the request (`io_recvzc`). The source shows these: `-ENOSPC` when the
  completion ring had no room and nothing was delivered in that call
  (`io_zcrx_recv_frag`, `io_zcrx_copy_chunk`); `-ENOMEM` when the copy
  path found no free page and nothing was delivered (K4); `-EFAULT` (K5);
  the socket's own errors. When something was delivered before the
  shortage, the call returns the bytes and the request goes on. After
  twenty socket buffers in one call the kernel requeues the request
  itself (`IOU_REQUEUE`); the program sees nothing of it.
- **K4 Copying behind the same face.** Payload that does not lie in the
  area (the linear part of a packet, a flow that reached another queue of
  the card) is copied by the kernel into a page taken from the area's free
  list and completed like any other piece. The free list is not the pool's
  cache and not the refill ring: it can be empty while the area is far
  from empty. A program cannot tell a copied piece from another.
- **K5 Another ring's pages.** A fragment that lies in an area registered
  to another ring is not copied: the receive ends with `-EFAULT`.
- **K6 The refill ring.** At most 32,768 entries, a power of two. The
  kernel takes up to a batch when the card's pool wants pages, and counts
  what is there as tail minus its own head, capped at the ring's size: a
  tail written past a full ring makes it read slots twice, which is as
  many double returns (K1). `IORING_REGISTER_ZCRX_CTRL` with
  `ZCRX_CTRL_FLUSH_RQ` takes every entry in one system call and stops only
  at an entry that names no page. In the runs the ring never held more
  than 1,219.
- **K7 An empty area.** The pool hands the card nothing
  (`io_pp_zc_alloc_netmems` returns none). In the one run that emptied the
  area for a second, the card dropped (365 `rx_out_of_buffer`), the sender
  sent 377 segments again, and data came again 5.4 seconds after every
  page was returned. That run did not check the stream's content or
  order, and what sets the delay is not known. Bytes waiting in a socket's
  receive queue lie in pages of the area too.
- **K8 Refusals of the registration.** `EPERM` without `CAP_NET_ADMIN`;
  `EINVAL` without deferred task work or without 32-byte or mixed
  completions, for a refill ring above 32,768 entries, and (from the
  queue's side, seen in the run, not in the saved source) without header
  and data split; `EOPNOTSUPP` for pages larger than the system's (run);
  `ENODEV`, `ERANGE` and the charge of the area against the memory-lock
  limit (source). The builder reads the card's split setting over netlink
  first, since `EINVAL` alone does not say which.
- **K9 Dropping the ring** takes back every page a program holds
  (`io_zcrx_scrub`); pages in socket queues go back when those sockets do.
- **K10 Not shown.** What a ring is told when the operator reconfigures
  the queue or takes the interface down: the source's `io_pp_uninstall`
  posts nothing (task 3.1). Whether an ordinary receive works on a socket
  whose packets lie in the area (not needed, C1).

### C3 The units
- A **page** is 4,096 bytes of the receive area. The driver grants no
  other size (run).
- A **piece** is what one completion delivers: a run of bytes in one
  page. `Completion` gains the offset of a piece in its page
  (`Completion::off`, *new*); its page is `Completion::buf`, a `BufId` of
  a new class of the pool, as a ring buffer is. `Io::give_back` returns
  one piece: one reference of K1.
- A **frame in pages** (`Mem::Pages`, *new*) is a frame whose whole blocks
  are lent in the pages they arrived in, and whose last, partial block
  lies zero-padded in a block-aligned **tail block** of its own (a pool of
  4 KiB buffers, one a frame in pages, *new*). It is named by a **run**:
  an entry of a table sized at build (`Sizes::zc_frames`, *new*) that
  lists the frame's pieces, with a generation and a count of holds.
- A **moved frame** is a frame in frame memory (`Mem::Extent`): what
  direct mode gives. Every frame of the mode is one or the other.

### C4 When a frame is lent in its pages
The log wants each frame at a block boundary, padded to whole blocks. So
a frame is **in step** when byte `x` of the frame lies at offset `x mod
4096` of a page, and each of its blocks lies in one page. That is so when
the frame's first byte starts a page, which it does when the frame starts
a packet whose payload the card laid at the start of a page, and it stays
so while every packet inside the frame carries a whole number of pages.

The connection reads as direct mode does, in two armings a frame:

1. **The header.** Armed with a credit of the header's length (less what
   is in the header area already). Its pieces are read where they lie.
   The header is copied to the header area (`hdr_have`), as today.
2. **The decision**, when the header is whole (`begin_frame`). The
   connection takes its frame memory first, as today: the frame's
   **landing**, held as `asm`. No frame memory: it waits (`W_READ`),
   holding its header's pieces, which are within its credit. With a
   landing, the frame is *kept* if all of these hold, and *moved* if not:
   its header lay in one page from offset 0; it is at least
   `zc_min_frame` long (*new*, default one page); a run is free; a tail
   block is free; the page budget (C5) has room for its whole blocks.
3. **The body.** Armed with a credit of exactly the rest of the frame.
   For a kept frame, a piece that continues in step is recorded in the
   run; the piece or pieces of the last, partial block are copied to the
   tail block and returned in the same turn. For a moved frame every
   piece is copied to the landing at `asm_have` and returned in the same
   turn.
4. **A break.** A piece of a kept frame that does not continue in step
   (another offset than the frame's, a block in two pages, no piece
   record left of `Sizes::zc_pieces`, *new*) makes the frame a moved one
   on the spot: its
   kept pieces are copied into the landing in order and returned, and the
   rest follows as in 3. The landing is always there, so a break never
   waits.
5. **The end.** A kept frame becomes `ready` as a run and its landing is
   released (or kept for the connection's next frame, as its one buffer).
   A moved frame becomes `ready` in its landing, as in direct mode.

A page that holds the end of one frame and the start of the next is never
kept: the first frame's last block goes to its tail block, and the second
frame does not start a page, so it is moved. Hence **only whole pages,
each holding one whole block of one frame, are ever held past a turn.**

`HeaderFraming::with_body_at_block` does not combine with the mode
(`NetError::Framing`): a frame in pages lies with its header at a block.

### C5 The credit and the budgets
- **The credit is the frame.** A connection's receive is armed for the
  rest of its header or the rest of its frame, never more (K2). Every
  byte the kernel can deliver under it has a place before it is asked
  for: the header area, or the landing. So nothing delivered ever waits
  for memory, and no piece that is not kept outlives the turn it came in.
- **The page budget** (`Sizes::zc_pages`, *new*) bounds the pages kept by
  all frames in pages of a `Net`, arriving or lent. A frame takes its
  whole blocks from it at the decision and gives them back when its run
  is freed or it breaks. No room: the frame is moved. The budget never
  makes a connection wait.
- **Why nothing waits for ever.** Thirty-two connections each part-way
  through a frame: each holds its landing or waits for one before its
  body is armed, exactly as in direct mode; kept pages are not what any
  of them waits for. A connection the caller does not pull: it holds one
  `ready` frame and is not armed again (`read_direct` returns while
  `ready` is set), so it holds one frame's pages or one landing and its
  window closes in the kernel. A frame sent to a peer that reads slowly:
  a frame in pages cannot be queued for sending (C6), so only frame
  memory waits on a slow peer, under today's rules.
- **`held_max` and `pause`.** A frame in pages counts its length against
  `held_max` like any delivered frame. A paused connection is not armed
  again; `pause` does not cancel a credit in flight, whose bytes have
  their place. `dry` is ring mode's and is never set here.
- **Ends of an armed receive** (`on_recv_ended`): 0 with the credit used:
  go on; 0 with less: the stream ended, inside a frame or between frames
  as today; `-ECANCELED`, `-ENOSPC`, `-ENOMEM`: armed again on the next
  `poll` for what is left of the credit, the last two counted in `Usage`
  (C7); anything else ends the connection (`Why::of`).
- **Within a turn** the pieces out are at most the completion ring's
  entries: each is kept or returned before the turn's completions are
  done with.

### C6 Holds, `Frame`, and writing a frame in pages
- **Holds are counted, as for frame memory.** A run has the caller's hold
  from `next_frame` to `release`, and one more for each file write that
  takes one with `net.pages().hold(run)` (*new*, the twin of
  `net.extents().hold(..)`), released when its write and sync are done.
  The run's pieces are returned, its tail block and its share of the page
  budget freed, when the last hold goes.
- **`Frame` stays `Copy`.** A frame in pages carries its run's index and
  generation. `release`, `bytes`, `pages` and `hold` of a `Frame` whose
  generation is not the run's do nothing or say `Stale`, as a stale
  extent's do today. A second `release` of the same copy is such a call.
- **`release` is required for every frame of the mode**, as it is for
  every frame of direct mode, whose frames are all in frame memory. A
  program that is correct in direct mode releases every frame and stays
  correct. What changes for it is reading: `Net::bytes` is `None` for a
  frame in pages, since it is not one run of memory.
- **Reading.** `Net::pages(&frame)` (*new*) yields the frame's spans in
  order: its whole pages, then its tail block. The header lies in the
  first. `Frame::span` and `Frame::extent` are `None` for a frame in
  pages (*a change of their types*).
- **`hold`** of a frame in pages moves it once into frame memory and
  frees the run; `NetError::NoMemory` without frame memory, the frame
  staying as it is.
- **Writing.** `Op::FileWriteSpans` (*new*) writes a list of spans of the
  pool, as one vectored write, to a file at an offset: the list lies in a
  pool buffer the caller names (twelve bytes a span) and the backend
  turns it into the kernel's vector in a table sized at build
  (`Limits::gather_ops`, `gather_spans`, *new*). It completes like
  `Op::FileWrite`. With direct I/O every span is whole blocks, which a
  frame in pages gives: whole pages and a padded tail block. The caller
  holds the run until the write and its sync complete.
- **Sending a frame in pages is refused** in this change:
  `send_lent` says `SendError::NotHeld`, as for a frame lent from the
  ring. A frame is some 256 pages, a connection queues `spans` sends, and
  whether the area can back a zero-copy send is not known. A caller that
  relays a frame calls `hold` first. Task 3.5 tries a send from the area;
  what it finds decides a later change.

### C7 What `Config::plan` checks, and what it cannot
It checks what it knows: the area holds at least `zc_pages` (kept), the
completion ring's entries (within a turn), and `zc_spare` pages (*new*)
that the operator gives for what the kernel holds; runs, tail blocks and
the piece records fit their counts; the refill ring and the backend's
queue of returns each hold every piece that can be out (`zc_pieces` plus
the completion ring's entries), at most 32,768.

It cannot know what the kernel holds: the card's own ring and pool cache,
and what waits in sockets' receive queues, which the kernel accounts by a
measure of its own. `docs/tcp.md` gives a rule of thumb for `zc_spare`
from the card's ring and the connections' receive buffers; it is advice,
not a guarantee. When the area runs dry all the same, nothing is lost for
the program (K7), it is slow, and `Usage` says so as far as the program
can see: `area_short` (*new*) counts receives that ended with `-ENOMEM`.
Packets the card dropped show only in the card's own counters.

### C8 A queue's registration
| State | Entered | Left |
|---|---|---|
| None | a backend built without a receive area | never |
| Registered | at build; refusals are build errors (K8) | the backend dropped (K9) |
| Lost | the operator reconfigures the queue or takes the interface down | not known (K10): task 3.1 establishes what a ring sees, and this row is written then. Until it is, the mode documents that the interface is not to be reconfigured under a running backend |

### C9 Where a piece can be
| Where | Who may read it | Leaves by |
|---|---|---|
| Arrived this turn, not yet dealt with | the set | being kept, or being copied and returned, before the turn's completions are done |
| Holding the header a connection waits to decide on (no frame memory) | the set | the decision; the connection ending (returned) |
| Kept: listed in the run of a frame that is arriving | the set | the frame's end (lent); a break (copied, returned); the connection going down or closing (returned) |
| Lent: listed in the run of a delivered frame | every holder of the run, through `pages` | the last hold's release (returned); `hold` (copied, returned) |
| Returned, waiting in the backend's queue | nobody | room in the refill ring |
| Returned, in the refill ring | nobody | the kernel taking it |

A connection that goes down to dial again, closes, or fails gives up what
it holds for a frame not delivered, as `drop_rx` does today: the run of
the arriving frame with its pieces, its landing, its header's pieces.
Frames already delivered stay with their holders. A `Net` is dropped
after its frames are released, as today; one dropped earlier leaves its
lent pieces with the kernel's count until the backend is dropped (K9).

### C10 Invariants
Each is checked after every step of the model test; "sim" says how the
simulator's area (C12) sees a violation.

- **Z1 Once.** Every piece delivered is returned exactly once. Sim: it
  keeps K1's count a page and the set of pieces out; a return of a piece
  not out fails the run at once.
- **Z2 Not after.** No byte of a piece is read after its return. Sim: it
  overwrites the piece's own bytes with a marker at `give_back`, whatever
  else the page still backs, and the model reads every frame it holds
  again, late, against what was sent.
- **Z3 In order.** A frame's bytes, through `pages` or after `hold`, are
  the bytes sent, in order. Sim: the model compares.
- **Z4 Whole pages only.** Every piece listed in a run of a delivered
  frame, with the others of its page, covers the page from 0 to 4,096.
  Checked on the runs.
- **Z5 Nothing past the turn but kept pages.** After `poll`, every piece
  out is in a run or holds a header that waits for frame memory. Sim: its
  set of pieces out is compared with the runs'.
- **Z6 Budgets.** Pages kept never exceed `zc_pages`; runs, tail blocks
  and piece records never exceed their tables.
- **Z7 The ring is never overrun.** The refill ring's tail is never more
  than its size ahead of its head. Sim: its refill ring is small, read
  lazily, and fails the run on such a tail.
- **Z8 At rest.** With every frame released and every connection closed,
  no piece is out, the page budget is whole, both queues of returns
  drain, and `Usage::is_idle` holds.
- **Z9 After close.** A frame of a closed connection is readable until
  its last hold is released, and its pieces are returned then.
- **Z10 Moves are counted.** `Usage::moved_bytes` (*new*) counts every
  byte the set copies on receive. A frame delivered in pages adds only
  its header and its tail block; a moved frame adds its length once.
- **Z11 At build.** Nothing is allocated for the mode after build (the
  allocation guard; the area itself is mapped at build).
- **Z12 The same contract.** I1 to I8 of `docs/tcp.md` hold in the mode,
  I6 in particular: the bytes pulled on a connection are the bytes sent,
  whatever the pieces were. (Traces differ between modes; they are not
  compared.)

### C11 What runs out, and what then
| Runs out | What happens | Ends when |
|---|---|---|
| Frame memory for a landing | The connection is not armed for its body; it waits (`W_READ`), as in direct mode | a frame is released |
| The page budget, runs, or tail blocks | The frame is moved instead of kept; `Usage::frames_moved` (*new*) counts it | at once |
| Piece records (`zc_pieces`) | The frame breaks and is moved (C4) | at once |
| `held_max` | The connection is not armed again (`paused`) | `release` brings it below |
| Room in the refill ring | Returns wait in the backend's queue; `tick` writes what fits and asks the kernel to take them (K6), repeating until the queue is empty | the same `tick` |
| Room in the completion ring | The receive ends with `-ENOSPC` (K3) and is armed again on the next `poll` for the rest of its credit; `Usage::rearmed` (*new*) counts it | the next `poll` |
| Free pages for the kernel's copy | The receive ends with `-ENOMEM` (K4) and is armed again on the next `poll`; `Usage::area_short` counts it | pages are returned |
| Pages in the area | The card drops and TCP repairs (K7); kept from happening by C7 as far as the program can; seen in the card's counters | pages are returned, and some time after |

### C12 The simulator's model
`SimIo` gets an area of pages and delivers `Op::ArmedRecvZc` (*new*) as
pieces, up to the receive's credit and the room in its completions:

- pieces of seeded length, from one byte to a page; a page delivered as
  two or more pieces (the split first page of the run); a new piece
  starting a new page or, seeded, continuing the last one;
- frames that start a page and frames that start inside one, and a short
  piece in the middle of a frame, so that kept, moved and broken frames
  all occur;
- now and then a piece through the copy path (K4), taken from a free list
  of its own that can be empty: `-ENOMEM`;
- a completion ring it finds full, seeded: `-ENOSPC`;
- the end of a credit and the end of the stream, both with result 0;
- K1's count a page, the set of pieces out, the marker of Z2;
- a refill ring of seeded, small size, read lazily, with the flush (Z7);
- nothing delivered while no page is free, for a seeded time longer than
  the usual delays (K7).

It does not model `-EFAULT` (K5), which ends a connection like any other
receive error and is covered by the existing reset campaign, nor `Lost`
(C8).

## Risks / Trade-offs
- The driver may not support it on the Sparks → the spike of group 1 is
  first and is allowed to end the change.
- A queue given to one ring is taken from the kernel's RSS → documented;
  the refusal names an unsteered queue where it can be detected.
- `CAP_NET_ADMIN` is needed to register → refusal names it.
- 32-byte completions double the completion ring's memory → only when the
  mode is asked for.
- Two armings a frame, as direct mode's two receives → the mode is for
  large frames; ring mode stays the one for small ones.
- A frame is lent in pages only when it starts a packet and its packets
  carry whole pages (MTU 8,244 on the Sparks) → the sender's and the
  operator's to arrange; any other frame costs what direct mode costs,
  with the kernel's copy replaced by ours.
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
- What a ring is told when its queue is reconfigured (K10, task 3.1).
- Whether the area can back a zero-copy send, so that a frame is relayed
  from its pages (task 3.5; a later change if so).
- What sets the delay after an empty area (K7); whether it matters once
  the area is sized as C7 advises.
- How often frames of a real replication stream start a packet, and so
  how many are lent in pages (task 5.1).
- Mixed completions (`IORING_SETUP_CQE_MIXED`) instead of 32-byte ones.
