# Change: longshore_tcp under simulation, tests and benchmarks

## Why
`add-tcp-crate` builds `longshore_tcp` and checks it on the real kernel and
the portable backend. Messaging and replication fail in the cases a
loopback test does not reach: resets mid-frame, partial reads, stalls,
peers that restart, corrupted bytes under TLS. longshore checks every
protocol path under seeded faults in the simulator; the TCP crate needs the
same before UltraGraphDB moves onto it. Its defaults (receive mode, ring
buffer size, zero-copy threshold, send segment size) are also to be chosen
by measurement, and the measurements set the baseline that change 3's
kernel bypass must beat.

The review of `add-tcp-crate` showed what its hand-written scenarios did
not reach: events in an awkward order (a close racing with a completion)
and pools empty at an awkward moment (frame memory gone when the stream
ends). About two dozen such faults were found by a reviewer, a round at a
time. This change makes them findable by a seed: it states `Net`'s state
tables and invariants (`design.md`) and checks them with a model test
under random operations, small pools and reordered completions, before
anything else is added.

Second of three changes (`add-tcp-crate`, this, `add-tcp-af-xdp-backend`).

## What Changes
- The contract first: the connection and listener state tables, twelve
  invariants and an answer for every empty pool (`design.md`); a model
  test (`tcp_model`) that checks them after every step of seeded random
  operations; the simulator delivering a tick's completions in a seeded
  order; a conformance suite for transports, which change 3 must pass.
- Two changes to the machine, found by checking its state tables against
  the code: an end event is held back until everything received has been
  pulled, so that no order of pulling loses frames; and half-close, as an
  option per connection, so that a peer that sends, shuts down and waits
  can be answered (`Event::Finished`). With them `NetError::NotUp`, one
  frame for a refused connection, and listener slots reused only when
  quiet. **BREAKING** for callers: `shutdown_write` on a connection that
  is not up returns `NotUp` where it returned `Stale`.
- Working method for this change and the next: tables and invariants
  before code, adversarial scenarios for every requirement, a review at
  the end of each task group.
- Simulation: `KernelTransport<SimIo>` under seeded network faults (resets,
  short writes, partial reads, delays, flipped bits, peers that close and
  listen again), with trace hashes per seed, covering framing, held frames
  and backpressure, reconnect, refusal, half-close and drain.
- Storage and network faults in one run: power loss with torn writes
  (the simulated disk) while frames are being received, logged from their
  extents and replicated, checking that nothing is acknowledged before it
  is durable and no extent is reused while held.
- A packet-level simulated device for the transport seam, so that change
  3's user-space TCP can be tested deterministically too (loss,
  reordering, duplication, delay at the packet level).
- Property tests (Hegel) for framing under arbitrary splits and lengths; a
  fuzz target `tcp_framing`; allocation checks over long simulated runs.
- Benchmarks: a `tcp_uring` tool and criterion benches measuring
  request/response latency (p50, p99, p999) at UltraGraphDB's frame sizes,
  one-way throughput per core with 64 KiB and 1 MiB frames, and connection
  scale to 4,096, each against a plain `epoll` baseline on the same host;
  and the whole path, wire to durable to replicated to answered, with the
  log written by write and sync against `O_DSYNC` writes on a preallocated
  file, and the turn sleeping against spinning.
- Defaults set from the results; results in `bench/results/`.

## Impact
- Affected specs: `tcp-connections` (ADDED: checked invariants, exhaustion,
  the end after everything received, half-close, errors, listener slots,
  transport conformance, simulation, measured defaults)
- Affected specs, added while it was built: `file-operations` (ADDED:
  durable writes) and `simulated-disk` (ADDED: files opened for durable
  writes), for the open flag the whole-path measurement needed
- Affected code: `longshore_tcp` tests and defaults, `longshore_common::sim`
  (packet device), `longshore_common::io` (`OpenFlags::with_dsync` on every
  backend), `longshore-bench` (`tcp_uring`, benches), `fuzz/`
- Depends on `add-tcp-crate`
