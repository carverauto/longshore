Each group ends with a review of its own diff (no-mistakes or
`/code-review`); its findings are fixed before the next group starts.

## 0. Decisions from checking the tables against the code
- [ ] 0.1 C1: half-close by the peer: support sending after the peer's end, or state that either end ends the connection; spec and table updated
- [ ] 0.2 C2: the end event and frames not yet pulled: hold the end while input is left, or state the caller's order in the contract
- [ ] 0.3 C3 to C7 fixed, each with a scenario test that fails before the fix

## 1. The contract, checked
- [ ] 1.1 Seeded completion order within a tick in `SimIo` (off by default), with its own tests
- [ ] 1.2 `tcp_model`: the adversary, the byte-queue reference and invariants I1 to I12 of `design.md` checked after every step; small pools; `--seed`, `--steps`, `--shrink`
- [ ] 1.3 Every cell of the connection and listener state tables is either reached by `tcp_model` (a coverage report by state and event says so) or shown unreachable by a debug assertion
- [ ] 1.4 Every row of the exhaustion table has a scenario test: the pool empty, the taker refused or paused, then woken without a further call
- [ ] 1.5 Flags to states: what a closing slot waits for, and why a connection does not read, as enums; no behaviour change, `tcp_model` unchanged and green
- [ ] 1.6 `longshore_tcp::check::conformance` for transports, run against `KernelTransport` on `SimIo`, `PosixIo` and (in `tcp_uring`) `UringIo`
- [ ] 1.7 Review of group 1

## 2. Fault campaigns
- [ ] 2.1 Seeded fault scenarios for `KernelTransport<SimIo>`: resets mid-frame, short writes, partial reads, delays, flipped bits, peers that close and listen again; each also under close, under exhaustion and at the end of the stream
- [ ] 2.2 Trace hashes per seed for framing, held frames and backpressure, reconnect, refusal, half-close and drain
- [ ] 2.3 One thousand seeds in CI's test run (fewer in the default `cargo test`), with `alloc_guard`; one hundred thousand `tcp_model` seeds nightly
- [ ] 2.4 Storage and network together: power loss and torn writes during receive, log-from-extent and forward; durable-before-answer and hold-count invariants over one thousand seeds
- [ ] 2.5 TLS under faults in both receive modes: flipped bits, a stream cut without a close alert, records arriving with the end, data behind the handshake, with frame memory exhausted at each of those moments
- [ ] 2.6 Review of group 2

## 3. Packet device
- [ ] 3.1 Its state table and invariants in `design.md` first (a packet is in exactly one place: queued, in flight, delivered, dropped)
- [ ] 3.2 Packet device in `longshore_common::sim` with seeded loss, reordering, duplication and delay, and its own tests
- [ ] 3.3 Review of group 3

## 4. Properties and fuzzing
- [ ] 4.1 Hegel properties: framing under arbitrary splits and lengths, both framing kinds; send coalescing preserves order and boundaries; extent hold counts never go negative and buffers are reused only at zero
- [ ] 4.2 Fuzz targets `tcp_framing` and `tcp_model_steps` (the adversary's steps from fuzzer bytes), added to `scripts/fuzz.sh` and the Fuzz workflow
- [ ] 4.3 Review of group 4

## 5. Benchmarks
- [ ] 5.1 `tcp_uring` tool: request/response with a 256-byte header and small bodies (latency p50/p99/p999), bulk one-way with 64 KiB and 1 MiB frames (GB/s and CPU per GB), connection scale to 4,096
- [ ] 5.2 The whole path (wire, durable, replicated, answered): write and sync against `O_DSYNC` on a preallocated file; sleeping against spinning turns
- [ ] 5.3 An `epoll` baseline in the same tool, and criterion benches for framing and send coalescing
- [ ] 5.4 Runs in the Linux container: both receive modes, ring buffer sizes, zero-copy thresholds, send segment sizes; results in `bench/results/`
- [ ] 5.5 Runs on physical hosts with 200 and 300 Gb/s NICs (by the colleague with access), results in `bench/results/`
- [ ] 5.6 Defaults set from 5.4 and 5.5; `docs/tcp.md` updated, with the state tables and invariants
- [ ] 5.7 Review of group 5
