Each group ends with a review of its own diff (no-mistakes or
`/code-review`); its findings are fixed before the next group starts.

## 0. The machine the tables describe
- [x] 0.1 `Finishing`: an end event is owed only when nothing received is left to pull; the reason is settled then; the flag that withdrew an owed end is removed (C2, C3)
- [x] 0.2 Half-close: `ConnOpts::with_half_close`, `HalfClosed`, `Event::Finished`; sending after the peer's orderly end until the caller's shutdown, close or a failed send (C1)
- [x] 0.3 `NetError::NotUp` for `shutdown_write` and for sends where the connection is not open for sending (C5); one frame for a refused connection (C4)
- [x] 0.4 A listener slot is `Free` only when its close has completed and no accept is in flight (C6)
- [x] 0.5 A scenario test for each of C1 to C6; those for C1, C2, C4 and C5 failed before their fix. The one for C6 and C8 guards the rule but did not fail before it: the simulator cannot show that fault until 1.1. The shared checks gained the half-close exchange and listener slots coming back
- [x] 0.6 `docs/tcp.md` and the crate docs: the end of a connection, half-close, the order of events
- [x] 0.7 Review of group 0: ten findings, each closed in a commit of its own. Three were faults (a connection parked under a closing listener reaching the next one, the idle deadline cutting off a half-close answer still being sent, a failed shutdown reported as orderly); only the second could be reproduced by a test

## 1. The contract, checked
- [x] 1.1 Seeded order for the reports of a closed socket's operations in `SimIo` (`Faults::with_reorder`, off by default), with its own tests. With it the model reproduces what group 0 could only guard: reintroducing the early free of a listener slot, or dropping the parking of an accepted connection, fails
- [x] 1.2 `tcp_model`: the adversary, the frame-carried reference and the caller-visible invariants checked as the run goes and at its end; small pools; the example `tcp_model` with `--seed`, `--steps`, `--shrink`, `--trace`.
- [x] 1.3 `Net::coverage` counts, in debug builds, which event reached a connection in which state; the model test holds the sum over its seeds against the connection state table: every cell the table allows and the model can reach is reached, none the table excludes is.
- [x] 1.4 A scenario for every row of the exhaustion table: six existed, three are new, and `net/exhaustion.rs` names them all
- [x] 1.5 Flags to states where they exclude each other: the socket of a slot is one state; the reading flags and the redial flag stay, for the reasons in `design.md`; the model test unchanged and green
- [x] 1.6 `longshore_tcp::check::conformance`: a transport brings a rig, the suite checks what `Net` relies on. Run against `KernelTransport` on `SimIo` (200 seeds of reordered reports), `PosixIo`, and in `tcp_uring` on `UringIo` and `epoll`.
- [x] 1.7 Review of group 1: ten findings, each closed in a commit of its own. One was a fault in `Net` (a header that came in pieces still took a second frame buffer, reproduced by a scenario); one in the simulator (the reorder setting delayed named cancels too, and could put a close off for ever); the rest were holes in the model test, the conformance suite and the replay tool
- [x] 1.8 The open items of 1.2, 1.3 and 1.6, closed: the model reconnects, runs one run in three over TLS with a fixed key so that a failing seed replays, runs every call under `alloc_guard`, and takes an end as a protocol or TLS failure for a fault; `Net::coverage` and `KernelTransport::phase_coverage` count the listener table and the TLS phase table, and the model holds both; the conformance suite holds the busy refusal and `Secured`. Found on the way and fixed, each with a scenario: connections waiting on each other across the buffer ring and frame memory when there are more connections than frame buffers (C10), the TLS case of C9, and two faults in the fix itself that the model found over TLS (C11)

## 2. Fault campaigns
- [x] 2.1 Fault campaigns of the model test on `KernelTransport<SimIo>`: resets inside frames, receives and sends cut short, delays, refused connects, and bits flipped over TLS; listeners that close and listen again and connections that redial. The adversary's closes, the smallest pools and the end of each run put every fault under a close, under exhaustion and at the end of a stream. Not a scenario each: one adversary under each setting, 40 seeds a campaign in `cargo test`
- [x] 2.2 A trace hash per run, of every completion the backend delivered: the same seed gives the same hash and another seed another, in every campaign. The runs cover framing, held frames and backpressure, reconnect, refusal, half-close, and a drain at the end of half of them
- [x] 2.3 A thousand seeds in CI (`ci.yml`, an ignored test), a hundred thousand nightly on every core (`tcp-model.yml`), all under `alloc_guard`; `tcp_model --campaign NAME` replays one. A hundred thousand pass
- [x] 2.4 Storage and network together (`tests/tcp/storage/durable.rs`): a frame is written from its extent, synced, and only then forwarded to three peers, under network faults, with a power loss at a seeded turn; over a thousand seeds nothing a peer saw is missing from the log and the frame's memory is whole until its last hold goes. A log that answers before its sync is caught. Torn writes are the simulator's at a power loss; other disk faults (short, lost, misdirected writes) are not in it
- [x] 2.5 TLS under faults in both receive modes (`tests/tcp/tls/faults.rs`): a close alert, a stream cut without one, and a flipped bit, each with frame memory and with it exhausted; with the scenarios of records arriving with the end and of data behind the handshake that were there, and the `flips` campaign
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
