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

Second of three changes (`add-tcp-crate`, this, `add-tcp-af-xdp-backend`).

## What Changes
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
- Affected specs: `tcp-connections` (ADDED: simulation, measured defaults)
- Affected code: `longshore_tcp` tests and defaults, `longshore_common::sim`
  (packet device), `longshore-bench` (`tcp_uring`, benches), `fuzz/`
- Depends on `add-tcp-crate`
