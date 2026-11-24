## 1. Simulation
- [ ] 1.1 Seeded fault scenarios for `KernelTransport<SimIo>`: resets mid-frame, short writes, partial reads, delays, flipped bits, peers that close and listen again
- [ ] 1.2 Trace hashes per seed for framing, held frames and backpressure, reconnect, refusal, half-close and drain
- [ ] 1.3 One thousand seeds in CI's test run (fewer in the default `cargo test`), with `alloc_guard`
- [ ] 1.4 Packet device in `longshore_common::sim` with seeded loss, reordering, duplication and delay, and its own tests

## 2. Properties and fuzzing
- [ ] 2.1 Hegel properties: framing under arbitrary splits and lengths, both framing kinds; send coalescing preserves order and boundaries
- [ ] 2.2 Fuzz target `tcp_framing`, added to `scripts/fuzz.sh` and the Fuzz workflow

## 3. Benchmarks
- [ ] 3.1 `tcp_uring` tool: request/response with a 256-byte header and small bodies (latency p50/p99/p999), bulk one-way with 64 KiB and 1 MiB frames (GB/s and CPU per GB), connection scale to 4,096
- [ ] 3.2 An `epoll` baseline in the same tool, and criterion benches for framing and send coalescing
- [ ] 3.3 Runs in the Linux container: both receive modes, ring buffer sizes, zero-copy thresholds, send segment sizes; results in `bench/results/`
- [ ] 3.4 Runs on physical hosts with 200 and 300 Gb/s NICs (by the colleague with access), results in `bench/results/`
- [ ] 3.5 Defaults set from 3.3 and 3.4; `docs/tcp.md` updated
