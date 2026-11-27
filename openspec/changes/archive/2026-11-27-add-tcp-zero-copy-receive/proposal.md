# Change: io_uring zero-copy receive for longshore_tcp, before any user-space TCP

## Why
Between two DGX Sparks over 200 GbE (milestone 30) the receiving thread is
the bottleneck of kernel TCP: it fills its core at 43 to 58 Gb/s
(0.13 to 0.16 CPU-seconds a GB), while a thread that sends the same bytes
without copying them spends a sixth of that. Most of the difference is the
kernel's copy of every received byte into our buffer. The link itself is
filled without bypass, by four to eight receiving cores of twenty, so what
is left to win on these machines is those cores.

io_uring zero-copy receive (`zcrx`; Linux 7.0 for what this change uses) removes that copy
and keeps the kernel's TCP: the card writes payload straight into memory
the program registered, and the kernel processes only the headers. It is
an addition to the ring backend, not a second TCP stack to write and
verify. It therefore comes before `add-tcp-af-xdp-backend`, which is
now held until this change's numbers say whether a user-space stack would
still buy anything.

The same runs showed a fault in sending that loopback could not: zero-copy
sends of one 64 KiB segment each carry 0.7 to 1.2 GB/s over a card against
6.1 GB/s copied, with both ends idle. It is fixed here, because the
default it concerns was called provisional until measured on a card.

## What Changes
- **A spike first** (done, milestone 31). The Sparks' card takes the
  registration once header and data split is switched on. Measured with a
  test program, counting the receiving thread and the core that handles
  the queue's packets: half the CPU for payload that is not read on the
  way (0.07 against 0.14 CPU-seconds a GB), a tenth less for payload that
  is read; on the receiving thread alone a quarter of the cost, and three
  quarters of it. Chunks are one page of 4 KiB; the driver grants nothing
  larger.
- `Rx::ZeroCopy`, a third receive mode beside `Ring` and `Direct`, on the
  ring backend, for plain TCP. It is direct mode with one difference: a
  frame whose blocks arrive lying in whole pages of a receive area the
  backend registers at build is lent as those pages, and goes to a
  direct-I/O write without a copy. It is built for the frames a replica
  logs as received (UltraGraphDB's stage 8).
- A frame is lent in pages when it starts a packet and its packets carry
  whole pages; its last, partial block goes to a block of its own. Any
  other frame is moved once into frame memory and is then exactly what
  direct mode gives, at the cost of our copy in place of the kernel's,
  more completions and a dearer queue's core.
- The sending side of it: a connection can be asked to start every frame
  on a packet of its own, header and body in one send
  (`ConnOpts::with_frame_on_packet`). Frames on the wire are not padded,
  so without it almost no frame starts a page and the mode gains
  nothing; with it TCP may still cut or join packets, and such a frame
  is moved.
- Only whole pages are held past a turn, and the kernel holds the bound:
  a receive is armed with a byte credit for exactly the rest of a header
  or a frame, for which the connection already has a place.
- Frames in pages are read through `Net::pages`, held by the caller and,
  by count, by file writes, and written with a new vectored file write
  of the backend (`Op::FileWriteSpans`). `Net::bytes` is `None` for
  them, and sending one is refused: a caller that relays a frame calls
  `hold` first.
- **BREAKING**: `Frame::span` returns an `Option<Span>`, `None` for a
  frame in pages (it returns a `Span` today; `Frame::extent` is an
  `Option` already and is `None` for such a frame too). A program that
  does not use the mode takes what is always there.
- `Limits` and `Sizes` gain the area, its queue, the page budget, the
  runs and their tail blocks; all of it is allocated at build. A set
  with pages for the mode needs a frame buffer for every connection it
  can have, so the connections of the mode get a set of their own. The
  ring is set up with 32-byte completions only when the mode is asked
  for.
- Refusal, not fallback: on a kernel, driver, privilege or card setting
  that cannot do it, `Builder::build` fails with an error naming what is
  missing; TLS, Unix sockets, a shared port and a body asked for at a
  block are refused with the mode when a connection or listener is
  made. longshore does not configure
  the card; `docs/tcp.md` says what to set (header and data split, the
  queue kept out of RSS, the flow rule, the packet size).
- Simulation: `SimIo` delivers the mode's receives as pieces of pages,
  with seeded lengths, split pages, frames in and out of step, the
  kernel's copy path and every shortage, so the model test and the
  conformance suite cover the mode on every host.
- The body at a block, in direct mode: header framing can ask for a
  frame's body, not its header, at a block boundary of its memory.
  UltraGraphDB's log takes a bulk load's body that way; measured, it
  saves the move the database would make (6 % of the receiving thread).
  Built first.
- Zero-copy sends over a card: the cause of the slow case is found and
  the defaults (segment length, sends in flight a connection) are set so
  that no frame size sends slower with zero-copy than without.
- Measured on the Spark pair against the kernel-TCP baseline of milestone
  30, per receiving core and at the full link; the decision on
  `add-tcp-af-xdp-backend` is recorded with the numbers.

## Impact
- Affected specs: `tcp-zero-copy-receive` (ADDED), `tcp-connections`
  (ADDED: zero-copy sends over a card; a frame's body at a block; where
  a frame's bytes lie, by its kind)
- Affected code: `longshore_common` (`io::uring` registration, refill ring,
  `Op::ArmedRecvZc`, `Op::FileWriteSpans`, `Completion::off`,
  `Io::give_back_piece`; `sim`), `longshore_tcp` (`Rx::ZeroCopy`, frames
  in pages, runs, the credit, a frame on a packet of its own when
  sending, `Frame::span`),
  `longshore-bench` (`tcp_uring`), `docs/tcp.md`
- Kernel: the ring backend keeps Linux 6.1. This mode's floor is 7.0, the
  kernel it was read from and run on, and it refuses below it.
- Depends on `add-tcp-crate` and `add-tcp-simulation-and-benchmarks`.
  `add-tcp-af-xdp-backend` now depends on this change.
