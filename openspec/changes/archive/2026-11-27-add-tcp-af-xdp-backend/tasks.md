Each group ends with a review of its own diff (no-mistakes or
`/code-review`); its findings are fixed before the next group starts. No
code for a state machine is written before its table in `design.md` has
been reviewed.

## 0. The contract
- [x] 0.0 (closed 2026-11-27: milestone 35 left one case, a small message a packet, the traffic of order messages, not of a database; no case on large frames or batched small frames) The case restated from the results of `add-tcp-zero-copy-receive` (cores at the full link with and without the receive copy): go, or the change is closed
- [ ] 0.1 Review of the TCP connection, chunk ownership and queue socket state tables, the invariants X1 to X11 and the exhaustion table in `design.md`; corrections made there first
- [ ] 0.2 The packet-level model test's harness on change 2's packet device: both ends, seeded packet faults, the smallest pools, invariants checked after every step, replay by seed (failing until the stack exists)
- [ ] 0.3 The transport conformance suite of change 2 wired to run against `XdpTransport` on the packet device

## 1. Spike: user-space TCP
- [ ] 1.1 smoltcp over the packet simulator: interoperability gaps (window scaling, SACK, large windows, congestion control), GB/s per core
- [ ] 1.2 A minimal in-tree stack on the same harness, for comparison
- [ ] 1.3 Both candidates under the model test of 0.2: which cells of the state table each gets wrong
- [ ] 1.4 Decision recorded in `bench/results/` and `design.md`
- [ ] 1.5 Review of group 1

## 2. AF_XDP plumbing (longshore_common, Linux)
- [ ] 2.1 The `Io` pool on huge pages, NUMA-local, in 4 KiB chunks, registered as io_uring buffers and as UMEM; fill, completion, receive and transmit rings; zero-copy and copy modes; need-wakeup and busy polling
- [ ] 2.2 XDP program: steer the program's flows to the XSK map, pass everything else; loading and unloading
- [ ] 2.3 Multi-buffer frames for jumbo MTU; checksum offload where the driver has it
- [ ] 2.4 `Op::PollFd` (multishot poll on an outside descriptor) and the may-sleep rule in `Turn`
- [ ] 2.5 Refusal checks: driver zero-copy support, privileges, kernel features; errors naming what is missing
- [ ] 2.6 Chunk ownership as one accounted structure (X1), with a test for every row and transition of its table and for every row of the exhaustion table that concerns chunks and rings
- [ ] 2.7 Review of group 2

## 3. XdpTransport
- [ ] 3.1 `Transport` implementation: one socket per queue per core, connections, the one move from receive chunks into extents (with the caller's optional pass), extents and segments transmitted in place
- [ ] 3.2 User-space TCP from the spike: window scaling, SACK, timestamps, large windows, DCTCP with ECN and CUBIC, batched transmission
- [ ] 3.3 Flow placement: RSS-aware source ports for connects; flow rules or per-queue listeners
- [ ] 3.4 Neighbours and routes through netlink
- [ ] 3.5 TLS through `longshore_tls::record`
- [ ] 3.6 `close` and the linger table; memory held until acknowledged (X2); zero window and the window update on `release`
- [ ] 3.7 Every cell of the TCP state table reached by the model test (a coverage report by state and event) or asserted unreachable
- [ ] 3.8 Review of group 3

## 4. Tests
- [ ] 4.1 Packet simulator: loss, reordering, duplication, delay; same seed, same trace
- [ ] 4.2 Interoperability against Linux TCP peers on veth and on the NICs: long transfers under loss and reordering
- [ ] 4.3 Fuzzing of the packet parser; allocation guard
- [ ] 4.4 The change 1 API suite and the conformance suite run against `XdpTransport`
- [ ] 4.5 Each scenario of the spec delta under close, under exhaustion and at the end of the stream
- [ ] 4.6 One hundred thousand model seeds nightly
- [ ] 4.7 Review of group 4

## 5. Tuning on 200 and 300 Gb/s NICs
- [ ] 5.1 Baselines: kernel transport in ring, direct and zero-copy receive mode
- [ ] 5.2 Queues, ring and batch sizes, busy-poll budget, interrupt placement, huge page size, NUMA placement, core pinning; profiles per step
- [ ] 5.3 Results and settings in `bench/results/`; support matrix and tuning guide in `docs/tcp.md`
- [ ] 5.4 Review of group 5
