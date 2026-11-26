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
  without a copy, on the ring backend; a frame that cannot be lent so
  costs one move of ours where direct mode has the kernel's;
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
  frame. A frame that is moved is not free of cost against direct mode:
  our copy takes the place of the kernel's, with more completions and a
  dearer queue's core (Risks).
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
write-ahead-log spec (`../ultragraphdb/openspec/specs/write-ahead-log/
spec.md`, line 37: "A round SHALL be written as its frame, header and
body, at a 4,096-byte boundary, padded to whole blocks, in one write").
It replaces the contract that did not pass, and was itself reviewed: it
passed with corrections, which are made below, and with three points
(K3's partial case, the copy path's free list, the credit itself) to be
settled by the runs of task 2.0 before group 3. Names marked *new* do not
exist yet; every other name is the code's.

### C1 Scope and refusals
The mode is `Rx::ZeroCopy` (*new*), a third receive mode of a connection
or a listener. It is direct mode with one difference: where a frame's
blocks arrive lying in whole pages, the frame is lent as those pages
instead of being moved into frame memory.

| Asked for | Answer | Where |
|---|---|---|
| A backend with a receive area on the portable backend, on a kernel below 7.0 or without the flush of returns, without `CAP_NET_ADMIN`, on a queue without header and data split, or with an area the memory-lock limit does not allow | An `io::Error` naming which (C2, K8); no builder builds an area, so there is no `BuildError` | `UringIo::new`, `PosixIo::new` |
| `Rx::ZeroCopy` on a `Net` whose backend has no receive area, or whose `Sizes` have no pages for it | `NetError::Framing`, as for any option the set was not sized for (`fits`) | `connect`, `listen` |
| `Rx::ZeroCopy` with TLS | `NetError::NoTls` | `connect_tls`, `listen` with `with_tls` |
| `Rx::ZeroCopy` on a Unix-domain address | `NetError::Framing` | `connect`, `listen` |
| `Rx::ZeroCopy` with `Framing::Bytes` | `NetError::Framing`: the mode is of frames | `connect`, `listen` |
| `Rx::ZeroCopy` on a listener with `with_reuse_port` | `NetError::Framing`: a port shared between rings lets the kernel hand a connection to a ring whose queue its packets do not reach (K5) | `listen` |
| `Rx::ZeroCopy` with `HeaderFraming::with_body_at_block` | `NetError::Framing`: a frame in pages lies with its header at a block (C4) | `connect`, `listen` |
| Pages for the mode (`zc_pages` above zero) with fewer frame buffers than the set has connections | `ConfigError` naming the frame count (C5, "A landing for every connection") | `Config::plan` |

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
  its offset within the page or its length; an entry whose padding is not
  zero or that names another area than area 0 is refused; an entry for a
  page with no reference is skipped; one entry too many for a page takes
  away another completion's reference.
- **K2 The credit.** `IORING_OP_RECV_ZC` is multishot only, and its
  `sqe->len` is a count of bytes: the request delivers at most that many
  and then completes with result 0 and no more to follow (`io_recvzc`:
  `len && zc->len == 0`). End of stream completes the same way, with
  result 0. The program tells them apart by its own count: a receive that
  ends with 0 after delivering exactly its credit ended for the credit;
  one that ends with 0 after less saw the end of the stream. A credit of
  0 is no credit: the kernel reads it as "no bound" (`len ? len :
  UINT_MAX`), so a receive of the mode is never armed with 0 (Z13). The
  credit's end takes precedence over an error of the same call. Run
  (milestone 31, task 2.0): over 4,000 frames at each of two packet
  sizes, every arming of 256 bytes and every arming of the rest brought
  exactly its credit and ended with result 0 and no flag, each frame's
  first bytes were its length, and the body went on in the header's
  page, 256 bytes in.
- **K3 What ends a receive.** Any negative result but `-EAGAIN` completes
  the request (`io_recvzc`). The source shows these: `-ENOSPC` when the
  completion ring had no room and nothing was delivered in that call
  (`io_zcrx_recv_frag`, `io_zcrx_copy_chunk`); `-ENOMEM` when the copy
  path found no free page and nothing was delivered (K4); `-EFAULT` (K5);
  the socket's own errors; and `-EPROTONOSUPPORT`, `-ENOTSOCK`,
  `-ENOTCONN` and `-EINTR`, none of which the mode can meet on a
  connected TCP socket but all of which end the connection if they come.
  After twenty socket buffers in one call the kernel requeues the request
  itself (`IOU_REQUEUE`); the program sees nothing of it.
  **Not settled by the saved source:** when the completion ring fills
  after some bytes of a call were delivered, the call returns those bytes
  and the request stays armed with no terminal completion. Whether it is
  run again without a new packet waking the socket is decided in
  `io_uring/poll.c`, which was not read. If it is not, a frame that lies
  whole in the socket is never finished while its sender waits for the
  answer. Run (milestone 31, task 2.0): it is not. With a ring of 64
  entries and 1 MiB whole in the socket of a sender gone quiet, the
  receive brought 904,004 and 812,824 bytes in two runs and then nothing
  for six seconds, with no last completion. C5's rule is therefore
  needed.
- **K4 Copying behind the same face.** Payload that does not lie in the
  area (the linear part of a packet, a flow that reached another queue of
  the card) is copied by the kernel into a page taken from the area's free
  list and completed like any other piece. The free list is not the pool's
  cache and not the refill ring: it can be empty while the area is far
  from empty, and pages returned through the refill ring go to the pool's
  cache, not to it. The source shows the list getting pages back only
  from the copy path's own returns and from the pool releasing a page.
  So a receive that ends with `-ENOMEM` may meet the same on being armed
  again, for as long as the pool keeps what it has. Run (milestone 31,
  task 2.0): with an area of 512 pages the copy path found no page at
  once and on each of 4,000 armings after; with 16,384 pages it brought
  12,352 pieces and then none; pieces returned through the refill ring
  were free for it again only after a flush (K6), and never without one
  while the registered queue was idle. So the backend flushes the refill
  ring before a receive that ended with `-ENOMEM` is armed again (C5),
  and C5 bounds the retries for the area that is simply too small. A
  program cannot tell a copied piece from another.
- **K5 Pages of another registration.** A fragment that lies in a page of
  another registration (another ring's, or a second one of the same ring)
  or of no pool of this registration is not copied: the receive ends with
  `-EFAULT`.
- **K6 The refill ring.** At most 32,768 entries, a power of two; more is
  `EINVAL` unless the ring was set up to clamp, which longshore's is not.
  The
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
  receive queue lie in pages of the area too. A second run (milestone 31,
  task 2.0) held an area of 512 pages whole for four seconds: a second
  connection of 100-byte messages got one through and then nothing, no
  receive ended with an error, and in the five seconds after every page
  was back nothing came on either connection. So an empty area stops
  every flow of the queue, without a word to the program, for seconds
  beyond its return; a queue with no buffer posted may wait for
  something else to wake it.
- **K8 Refusals of the registration.** `EPERM` without `CAP_NET_ADMIN`;
  `EINVAL` without deferred task work or without 32-byte or mixed
  completions, for a refill ring above 32,768 entries, and (from the
  queue's side, seen in the run, not in the saved source) without header
  and data split; `EOPNOTSUPP` for pages larger than the system's (run);
  `ENODEV`, `ERANGE` and the charge of the area against the memory-lock
  limit (source). The builder reads the card's split setting over netlink
  first, since `EINVAL` alone does not say which.
- **K9 Dropping the ring** takes back every page a program holds
  (`io_zcrx_scrub`). That pages in socket queues go back when those
  sockets do is taken for granted, not shown by the saved source; the
  backend closes its sockets before its ring, so it does not rely on it.
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
  a new class of the pool, as a ring buffer is. A piece is returned with
  `Io::give_back_piece(buf, off, len)` (*new*): one reference of K1, named
  by page, offset and length, so that the backend and the simulator know
  which piece of a page went back and a wrong piece of the right page is
  seen (Z1, Z2). `Io::give_back`, which names only a buffer, stays ring
  mode's. The cost is twelve bytes a queued return in place of four; the
  kernel's own entry has room for the offset and length already.
- A **frame in pages** (`Mem::Pages`, *new*) is a frame whose whole blocks
  are lent in the pages they arrived in, and whose last, partial block
  lies zero-padded in a block-aligned **tail block** of its own (a pool of
  4 KiB buffers, one a frame in pages, *new*). It is named by a **run**:
  an entry of a table sized at build (`Sizes::zc_frames`, *new*) that
  lists the frame's pieces, with a generation, the caller's hold (one
  bit) and a count of other holds (C6).
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
   **landing**, held as `asm`. No frame memory: it waits (`W_READ`). While
   it waits it holds at most one piece: a header that came as one piece
   from offset 0 of its page is noted in the connection's **header
   record** (*new*, one a connection) and stays out; the pieces of any
   other header (in two pages, or not from offset 0) cannot belong to a
   kept frame and go back at once, the header being in the header area
   already. With a landing, the frame is *kept* if all of these hold, and
   *moved* if not:
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
   released at once. A moved frame becomes `ready` in its landing, as in
   direct mode.

A page that holds the end of one frame and the start of the next is never
kept: the first frame's last block goes to its tail block, and the second
frame does not start a page, so it is moved. Hence **only whole pages,
each holding one whole block of one frame, are ever held past a turn.**

`HeaderFraming::with_body_at_block` does not combine with the mode
(`NetError::Framing`): a frame in pages lies with its header at a block.

**What the sender must do.** Frames on the wire are not padded
(UltraGraphDB's are any length from 256 bytes to 1 MiB), so in a stream
of frames back to back the next one starts wherever the last ended in its
page, and is moved. A frame is kept only if its sender starts it on a
packet of its own, with header and body in one send. That is the sending
side's work (task 3.9): `ConnOpts::with_frame_on_packet` (*new*) makes a
connection send a frame only when nothing of an earlier frame is still
unsent in the socket (it waits for the queue of that socket to drain,
which it sees as the earlier send's completion with nothing left over),
and queue header and body as one send. It cannot guarantee the packet
boundary: TCP may still join the frame's first bytes to a segment it
sends again, or cut a packet short inside the frame when its window or
its congestion state says so. Such a frame is moved; nothing is wrong.
Without this option on the sender the mode keeps almost no frame and is
direct mode at more cost; task 5.1 measures the share with and without.

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
- **A landing for every connection.** `Config::plan` refuses a set with
  pages for the mode (`zc_pages` above zero) unless it has at least as
  many frame buffers as connections (connects and accepts together). A
  connection of the mode holds at most one landing. So when one waits
  for a landing, the others of the mode hold fewer landings than there
  are frame buffers, and at least one frame buffer is held by something
  that does not wait for the receive area: a frame the caller holds or
  has not pulled, or a frame a connection of another mode is putting
  together through the kernel's ordinary receive. The wait then ends by
  a `release`, a pull, or that frame's completion, none of which needs a
  page. Without the rule there is a cycle direct mode does not have:
  with fewer frame buffers than connections, those without a landing are
  not armed, their senders fill the sockets' receive queues, every byte
  of which lies in a page of the area, the card is left without pages,
  the armed connections cannot finish their frames, and no landing is
  ever released. With it, the unarmed connections of that sequence do
  not exist: every connection that has a header has or gets a landing
  from a holder that needs no page. The price is frame memory: a set of
  many connections and large frames needs a frame buffer each (Risks).
- **Why nothing waits for ever.** Thirty-two connections each part-way
  through a frame: each holds its landing (the rule above), and kept
  pages are not what any of them waits for. A connection the caller does not pull: it holds one
  `ready` frame and is not armed again (`read_direct` returns while
  `ready` is set), so it holds one frame's pages or one landing and its
  window closes in the kernel. A frame sent to a peer that reads slowly:
  a frame in pages cannot be queued for sending (C6), so only frame
  memory waits on a slow peer, under today's rules.
- **`held_max` and `pause`.** A frame in pages counts its length against
  `held_max` like any delivered frame, once: `hold` of it moves the count
  with the frame and does not add to it. A paused connection is not armed
  again. `pause` cancels an armed receive today (`rx.rs`); for a
  connection of the mode it does not (a change task 3.3 names): a credit
  in flight has a place for every byte it can bring. `dry` is ring mode's
  and is never set here.
- **Ends of an armed receive** (`on_recv_ended`): 0 with the credit used:
  go on; 0 with less: the stream ended, inside a frame or between frames
  as today; `-ECANCELED`, `-ENOSPC`, `-ENOMEM`: armed again on the next
  `poll` for what is left of the credit, which is never 0 (a receive
  whose credit is used ended with 0, not with these); the last two are
  counted in `Usage` (C7); anything else ends the connection (`Why::of`).
- **`-ENOMEM` is retried a bounded number of times.** Sixteen ends with
  `-ENOMEM` in a row on one connection, with no byte delivered to it in
  between, end the connection with `Why::of(-ENOMEM)`, as any receive
  error does. Returning pages does not refill the list the copy path
  draws from (K4), so without the bound a connection whose payload comes
  by the copy path could be armed for ever against an empty list. Every
  such end adds to `Usage::area_short`, which therefore says "the copy
  path found no page", not "the area is empty"; `docs/tcp.md` says so.
- **A full completion ring part-way through a credit.** A turn in which
  the backend found the completion ring full is reported by the backend
  (`Io::ring_was_full`, *new*, true until the next `tick`). On such a
  turn the set cancels every receive of the mode that is armed with
  credit left, one cancel a receive as for any armed receive (the
  `cancelling` flag), and arms it again for the rest when the cancel's
  end comes. This answers K3's unsettled case: if the kernel does leave
  such a receive silent until a packet wakes the socket, the re-arm
  delivers what lies in it. The run of task 2.0 showed the stall, so
  the rule stands. It costs a cancel and an arming a connection on each
  such turn and nothing else: K2's count makes the second arming
  continue exactly
  where the first stopped. `Config::plan` counts one more operation a
  connection of the set for those cancels.
- **Within a turn** the pieces out are at most the completion ring's
  entries: each is kept or returned before the turn's completions are
  done with.

### C6 Holds, `Frame`, and writing a frame in pages
- **Two kinds of hold.** A run has the **caller's hold**, one bit, set
  when `next_frame` delivers the frame, and a **count of other holds**,
  one for each file write that takes one with `net.pages().hold(run)`
  (*new*, the twin of `net.extents().hold(..)`), given back with
  `net.pages().release(run)` (*new*) when its write and sync are done.
  The caller gets the run of a frame from `Frame::run()` (*new*; `None`
  for a frame in frame memory). The run's pieces are returned, its tail
  block and its share of the page budget freed, and its generation
  raised, when the caller's bit is clear and the count is zero, and not
  before.
- **`Frame` stays `Copy`.** A frame in pages carries its run's index and
  generation. `release(frame)` clears the caller's bit and nothing else.
  A `release`, `hold`, `bytes` or `pages` of a `Frame` whose run has
  another generation, or whose caller's bit is already clear, does
  nothing or says `Stale`. So a second `release` of the same copy, or a
  `release` after `hold`, is stale even while a write still holds the
  run, and cannot take the write's hold away.
- **`release` is required for every frame of the mode**, as it is for
  every frame of direct mode, whose frames are all in frame memory. A
  program that is correct in direct mode releases every frame and stays
  correct. What changes for it is reading: `Net::bytes` is `None` for a
  frame in pages, since it is not one run of memory.
- **Reading.** `Net::pages(&frame)` (*new*) yields the frame's spans in
  order: its whole pages, then its tail block. The header lies in the
  first. `Frame::span` and `Frame::extent` are `None` for a frame in
  pages: `Frame::span` returns an `Option` from now on, a **breaking**
  change of its type that every caller in the workspace follows
  (`Frame::extent` is an `Option` already).
- **`hold`** of a frame in pages copies it once into frame memory, gives
  the new frame the caller's hold and clears the caller's bit of the
  run: it is a `release` of the run with a copy made first. The run's
  pieces go back only when no write holds it any longer, so a frame can
  be under a file write and held for relaying at once. The frame's
  length stays counted against `held_max` once. `NetError::NoMemory`
  without frame memory, the frame staying as it is.
- **Writing.** `Op::FileWriteSpans` (*new*) writes a list of spans of the
  pool, as one vectored write, to a file at an offset: the list lies in a
  pool buffer the caller names (twelve bytes a span) and the backend
  turns it into the kernel's vector in a table sized at build
  (`Limits::gather_ops`, `gather_spans`, *new*). It completes like
  `Op::FileWrite`. With direct I/O every span is whole blocks, which a
  frame in pages gives: whole pages and a padded tail block. The caller
  holds the run until the write and its sync complete (Z14); the backend
  reads the spans' memory until the write's completion, not only when it
  is submitted.
- **Sending a frame in pages is refused** in this change:
  `send_lent` says `SendError::NotHeld`, as for a frame lent from the
  ring. A frame is some 256 pages, a connection queues `spans` sends, and
  whether the area can back a zero-copy send is not known. A caller that
  relays a frame calls `hold` first. Task 3.5 tries a send from the area;
  what it finds decides a later change.

### C7 What `Config::plan` checks, and what it cannot
It checks what it knows: the area holds at least `zc_pages` (kept), one
page a connection (a header held while its connection waits for frame
memory, C4), the completion ring's entries (within a turn), and
`zc_spare` pages (*new*) that the operator gives for what the kernel
holds; runs, tail blocks and the piece records fit their counts; frame
buffers are at least the set's connections (C5); the refill ring and the
backend's queue of returns each hold every piece that can be out
(`zc_pieces`, one a connection, and the completion ring's entries), at
most 32,768.

It cannot know what the kernel holds: the card's own ring and pool cache,
and what waits in sockets' receive queues, which the kernel accounts by a
measure of its own. `docs/tcp.md` gives a rule of thumb for `zc_spare`
from the card's ring and the connections' receive buffers; it is advice,
not a guarantee. When the area runs dry all the same, nothing is lost for
the program, but every flow of the queue stops, unannounced, and stays
stopped for seconds after pages are back (K7): this is the failure of
the mode to size against above all others. With a landing for every
connection
(C5) no connection is left unarmed for want of frame memory another
arriving frame holds, so what the sockets hold is drained as callers
release. `Usage::area_short` (*new*) counts receives that ended with
`-ENOMEM`, which is the copy path finding no page (K4, C5), not the area
being empty. Packets the card dropped show only in the card's own
counters.

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
| Noted in a connection's header record: a header of one piece from offset 0, while the connection waits for frame memory | the set | the decision (kept in the run, or returned); the connection ending (returned) |
| Kept: listed in the run of a frame that is arriving | the set | the frame's end (lent); a break (copied, returned); the connection going down or closing (returned) |
| Lent: listed in the run of a delivered frame | every holder of the run, through `pages` | the caller's bit clear and the count of other holds zero (returned); neither `release` nor `hold` returns a piece while a write holds the run |
| Returned, waiting in the backend's queue | nobody | room in the refill ring |
| Returned, in the refill ring | nobody | the kernel taking it |

A connection that goes down to dial again, closes, or fails gives up what
it holds for a frame not delivered, as `drop_rx` does today: the run of
the arriving frame with its pieces, its landing, its header's pieces.
Frames already delivered stay with their holders. A backend outlives a
`Net`, and a `Net` has no driver at hand when it is dropped, so it cannot
return pieces then. `Net::give_back_all(d)` (*new*) returns every piece
the set's runs and header records list, whatever holds them, and frees
the runs; it is called before a `Net` of the mode is dropped while its
backend lives on, and frames of that set are not read afterwards. A
`Net` dropped without it leaves its pieces out until the backend is
dropped (K9): a second set on that backend then runs on a smaller area.
`docs/tcp.md` says so.

### C10 Invariants
Each is checked after every step of the model test; "sim" says how the
simulator's area (C12) sees a violation without trusting the set's own
counters.

- **Z1 Once.** Every piece delivered is returned exactly once. Sim: it
  keeps K1's count a page and the set of pieces out, each by page,
  offset and length; a `give_back_piece` of a piece not out, or of other
  bytes of the right page, fails the run at once.
- **Z2 Not after.** No byte of a piece is read after its return. Sim: it
  overwrites the piece's own bytes with a marker at `give_back_piece`,
  whatever else the page still backs, and the model reads every frame it
  holds again, late, against what was sent.
- **Z3 In order.** A frame's bytes, through `pages` or after `hold`, are
  the bytes sent, in order. Sim: the model compares.
- **Z4 Whole pages only.** Every piece listed in a run of a delivered
  frame, with the others of its page, covers the page from 0 to 4,096.
  Checked on the runs.
- **Z5 Nothing past the turn but kept pages.** After `poll`, every piece
  out is in a run or in a connection's header record. Sim: its own set
  of pieces out is compared with the runs' and the records'.
- **Z6 Budgets.** Pages kept never exceed `zc_pages`; runs, tail blocks
  and piece records never exceed their tables. Sim: the pages kept are
  recomputed after every `poll` as the distinct pages of its own set of
  pieces out, less those of header records, and compared with `zc_pages`
  and with the set's own count; the two must agree.
- **Z7 The ring is never overrun.** The refill ring's tail is never more
  than its size ahead of its head. Sim: its refill ring is small, read
  lazily, and fails the run on such a tail.
- **Z8 At rest.** With every frame released and every connection closed,
  no piece is out, the page budget is whole, both queues of returns
  drain, and `Usage::is_idle` holds. The same holds after
  `Net::give_back_all`, whatever was held.
- **Z9 After close.** A frame of a closed connection is readable until
  its caller's hold and every write's hold are gone, and its pieces are
  returned then.
- **Z10 A frame in pages lies in the area.** Every span `Net::pages`
  yields for a delivered frame but the last lies in the receive area,
  and the last in a tail block. Sim: it knows which buffers are the
  area's and checks the spans itself, so a copy the set made and did not
  count is seen as a span outside the area. `Usage::moved_bytes` (*new*)
  counts every byte the set copies on receive (a frame in pages adds its
  header and its tail block, a moved frame its length once); it is
  compared with what the simulator's check implies, not trusted alone.
- **Z11 At build.** Nothing is allocated for the mode after build (the
  allocation guard; the area itself is mapped at build).
- **Z12 The same contract.** I1 to I11 of `docs/tcp.md` hold in the mode.
  I6: the bytes pulled on a connection are the bytes sent, whatever the
  pieces were. I8's list of memory with exactly one owner gains runs,
  tail blocks and pieces. I10 and I11 in particular: whoever was refused
  for lack of room is told when there is room, and a caller that pulls
  and releases everything sees every frame arrive; the model's progress
  check runs in the mode with the fewest frame buffers `plan` allows.
  (Traces differ between modes; they are not compared.)
- **Z13 No credit of 0.** A receive of the mode is never armed with a
  credit of 0. Sim: `Op::ArmedRecvZc` with 0 fails the run.
- **Z14 Not under a write.** No piece of a run is returned while a file
  write that holds the run has not had its last completion. Sim:
  `Op::FileWriteSpans` reads its spans when it completes, not when it is
  submitted, and the model compares the file's bytes with what was sent;
  a page returned and refilled meanwhile shows as the marker or as
  another frame's bytes in the file.

### C11 What runs out, and what then
| Runs out | What happens | Ends when |
|---|---|---|
| Frame memory for a landing | The connection is not armed for its body; it waits (`W_READ`), as in direct mode | a frame is released |
| The page budget, runs, or tail blocks | The frame is moved instead of kept; `Usage::frames_moved` (*new*) counts it | at once |
| Piece records (`zc_pieces`) | The frame breaks and is moved (C4) | at once |
| `held_max` | The connection is not armed again (`paused`) | `release` brings it below |
| Room in the refill ring | Returns wait in the backend's queue; `tick` writes what fits and asks the kernel to take them (K6), repeating until the queue is empty | the same `tick` |
| Room in the completion ring, nothing delivered in the call | The receive ends with `-ENOSPC` (K3) and is armed again on the next `poll` for the rest of its credit; `Usage::rearmed` (*new*) counts it | the next `poll` |
| Room in the completion ring, part-way through a call | The receive stays armed and may be silent (K3, not settled); on a turn the backend found the ring full, the set cancels and arms again every receive of the mode with credit left (C5) | the cancel's end |
| Free pages for the kernel's copy | The receive ends with `-ENOMEM` (K4); the backend flushes the refill ring, and the receive is armed again on the next `poll`; `Usage::area_short` counts it; sixteen in a row with nothing delivered end the connection (C5) | the copy path's list has a page again (when: task 2.0), or the connection ends |
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
  of its own that returns do not refill and that can stay empty for a
  seeded time, also for longer than sixteen armings: `-ENOMEM`;
- a completion ring it finds full, seeded: `-ENOSPC` when nothing was
  delivered in the call, and, part-way through a call, a receive left
  armed and silent until it is cancelled and armed again or, seeded, a
  new packet arrives (K3);
- the end of a credit and the end of the stream, both with result 0; a
  credit of 0 fails the run (Z13);
- K1's count a page, the set of pieces out by page, offset and length,
  the marker of Z2;
- `Op::FileWriteSpans` on the simulated disk, reading its spans at its
  completion (Z14);
- a refill ring of seeded, small size, read lazily, with the flush (Z7);
- nothing delivered while no page is free, for a seeded time longer than
  the usual delays (K7).

It does not model `-EFAULT` (K5), which ends a connection like any other
receive error and is covered by the existing reset campaign; `Lost`
(C8), which task 3.1 establishes on the Spark; nor the card dropping
packets (C11's last row), which the kernel's TCP repairs below anything
the simulator's sockets show: its stand-in is the seeded time with
nothing delivered, and the row itself is reached only by the run of
milestone 31 and task 5.1.

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
  carry whole pages (MTU 8,244 on the Sparks) → the sender's side is
  built in this change (task 3.9) and the packet size is the operator's;
  any other frame costs our copy where direct mode has the kernel's,
  with more completions and a dearer queue's core. Without the sender's
  side the mode gains nothing.
- A set with pages for the mode needs a frame buffer for every
  connection it can have (C5) → with 1 MiB frames that is 1 MiB a
  connection, 4 GiB for 4,096 connections. The mode is for the few
  connections that carry replication; they get a set of their own on
  the thread's backend, and the many client connections another without
  pages.
- Three things the contract rests on were read from the kernel's source
  and not run: the credit, what a full completion ring does part-way
  through a call, and when the copy path's free list fills again →
  task 2.0 runs them on the Spark before any code of group 3, and the
  clauses that depend on them (K2, K3 with C5's cancel rule, K4 with
  C5's bound) are corrected from what it shows.
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
- What sets the delay after an empty area, and what ends it (K7): two
  runs show seconds; neither shows the cause.
- What the first call returns when nothing at all fits the completion
  ring (`-ENOSPC` by the source): not run.
- How often frames of a real replication stream start a packet, and so
  how many are lent in pages (task 5.1).
- Mixed completions (`IORING_SETUP_CQE_MIXED`) instead of 32-byte ones.
