## 1. Spike: user-space TCP
- [ ] 1.1 smoltcp over the packet simulator: interoperability gaps (window scaling, SACK, large windows, congestion control), GB/s per core
- [ ] 1.2 A minimal in-tree stack on the same harness, for comparison
- [ ] 1.3 Decision recorded in `bench/results/` and `design.md`

## 2. AF_XDP plumbing (longshore_common, Linux)
- [ ] 2.1 The `Io` pool on huge pages, NUMA-local, in 4 KiB chunks, registered as io_uring buffers and as UMEM; fill, completion, receive and transmit rings; zero-copy and copy modes; need-wakeup and busy polling
- [ ] 2.2 XDP program: steer the program's flows to the XSK map, pass everything else; loading and unloading
- [ ] 2.3 Multi-buffer frames for jumbo MTU; checksum offload where the driver has it
- [ ] 2.4 `Op::PollFd` (multishot poll on an outside descriptor) and the may-sleep rule in `Turn`
- [ ] 2.5 Refusal checks: driver zero-copy support, privileges, kernel features; errors naming what is missing

## 3. XdpTransport
- [ ] 3.1 `Transport` implementation: one socket per queue per core, connections, the one move from receive chunks into extents (with the caller's optional pass), extents and segments transmitted in place
- [ ] 3.2 User-space TCP from the spike: window scaling, SACK, timestamps, large windows, DCTCP with ECN and CUBIC, batched transmission
- [ ] 3.3 Flow placement: RSS-aware source ports for connects; flow rules or per-queue listeners
- [ ] 3.4 Neighbours and routes through netlink
- [ ] 3.5 TLS through `longshore_tls::record`

## 4. Tests
- [ ] 4.1 Packet simulator: loss, reordering, duplication, delay; same seed, same trace
- [ ] 4.2 Interoperability against Linux TCP peers on veth and on the NICs: long transfers under loss and reordering
- [ ] 4.3 Fuzzing of the packet parser; allocation guard
- [ ] 4.4 The change 1 API suite run against `XdpTransport`

## 5. Tuning on 200 and 300 Gb/s NICs
- [ ] 5.1 Baselines: kernel transport, with and without zcrx where available
- [ ] 5.2 Queues, ring and batch sizes, busy-poll budget, interrupt placement, huge page size, NUMA placement, core pinning; profiles per step
- [ ] 5.3 Results and settings in `bench/results/`; support matrix and tuning guide in `docs/tcp.md`
