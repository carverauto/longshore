# Change: io_uring zero-copy receive for longshore_tcp, before any user-space TCP

## Why
Between two DGX Sparks over 200 GbE (milestone 30) the receiving thread is
the bottleneck of kernel TCP: it fills its core at 43 to 58 Gb/s
(0.13 to 0.16 CPU-seconds a GB), while a thread that sends the same bytes
without copying them spends a sixth of that. The whole difference is the
kernel's copy of every received byte into our buffer. The link itself is
filled without bypass, by four to eight receiving cores of twenty, so what
is left to win on these machines is those cores.

io_uring zero-copy receive (`zcrx`, Linux 6.15 and later) removes that copy
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
- **A spike first.** Whether the Sparks' card and kernel take a zero-copy
  receive registration at all is not known: the kernel has
  `CONFIG_IO_URING_ZCRX`, but the driver shows no header and data split
  setting. Group 1 is a program of a hundred lines that registers a queue
  and receives; if it is refused there, the change stops at its write-up
  and the next host is tried.
- `Rx::ZeroCopy`, a third receive mode beside `Ring` and `Direct`, on the
  ring backend only. Payload arrives in chunks of a memory area the
  backend registers at build for one receive queue of one card.
- Frames read in pieces. A frame that lies in one chunk is lent in place,
  as in ring mode. A frame that spans chunks is lent as its pieces, in
  order, with no copy (`Net::pieces`); a caller that needs it in one
  block-aligned run (a direct-I/O write) calls `hold`, which moves it once
  into frame memory, as today. `Net::bytes` keeps returning one slice and
  is `None` for a frame in pieces.
- Chunks are owned and accounted like ring buffers: lent with a frame,
  returned to the kernel through the refill ring on `release`, and an area
  with no free chunk stops the card's queue, not the program.
- `Limits` and `Sizes` gain the area (chunks and their length) and the
  queue it is for; all of it is allocated at build. The ring is set up
  with 32-byte completions only when the mode is asked for.
- Refusal, not fallback: on a kernel, driver, privilege or card setting
  that cannot do it, `Builder::build` fails with an error naming what is
  missing. longshore does not configure the card; `docs/tcp.md` says what
  to set (header and data split, the queue kept out of RSS, the flow rule
  that steers the listener's port to it).
- Simulation: `SimIo` delivers `Rx::ZeroCopy` receives in chunks of the
  same area model, with seeded chunk boundaries and exhaustion, so the
  model test and the conformance suite cover the mode on every host.
- Zero-copy sends over a card: the cause of the slow case is found and
  the defaults (segment length, sends in flight a connection) are set so
  that no frame size sends slower with zero-copy than without.
- Measured on the Spark pair against the kernel-TCP baseline of milestone
  30, per receiving core and at the full link; the decision on
  `add-tcp-af-xdp-backend` is recorded with the numbers.

## Impact
- Affected specs: `tcp-zero-copy-receive` (ADDED), `tcp-connections`
  (ADDED: zero-copy sends over a card)
- Affected code: `longshore_common` (`io::uring` registration, refill ring,
  `Op::ArmedRecvZc`; `sim`), `longshore_tcp` (`Rx::ZeroCopy`, pieces,
  chunk accounting), `longshore-bench` (`tcp_uring`), `docs/tcp.md`
- Kernel: the ring backend keeps Linux 6.1. This mode states its own floor
  (6.15, and the driver's support) and refuses below it.
- Depends on `add-tcp-crate` and `add-tcp-simulation-and-benchmarks`.
  `add-tcp-af-xdp-backend` now depends on this change.
