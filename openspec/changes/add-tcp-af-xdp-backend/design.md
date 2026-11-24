## Context
After change 2, `longshore_tcp` runs kernel TCP through io_uring with
measured defaults, and a packet-level simulated device exists. The hosts
this targets have 200 and 300 Gb/s NICs (25 and 37.5 GB/s). Measured so
far on kernel TCP, one core: HTTPS upload 7.2 GB/s and plain 12 GB/s on an
M3 Max VM; on download the kernel's receive copy dominates.

## Goals / Non-Goals
- Goals: line rate on a 200 Gb/s NIC and as close as the hosts allow on
  300 Gb/s, with fewer cores and lower p99 latency than kernel TCP; zero
  copy from NIC to caller and back; the change 1 API unchanged.
- Non-Goals: Unix-domain sockets over AF_XDP (they stay on the kernel
  transport); sharing a connection between kernel TCP and the user-space
  stack; DPDK.

## Decisions
- **AF_XDP, not DPDK**: in-kernel drivers, no NIC unbinding, other traffic
  stays with the kernel through `XDP_PASS`.
- **One socket per queue per core**: the core that owns a queue owns its
  rings, its connections and its share of UMEM; no locks, as everywhere in
  longshore. UMEM is per core unless a shared UMEM measures better.
- **Zero copy by default, copy mode on request**: copy mode exists for
  development hosts and VMs; production refuses rather than degrade
  silently.
- **Flow placement**: on connect, choose a source port whose RSS hash lands
  on the owning core's queue (Toeplitz hash computed with the NIC's key);
  for listeners, flow rules or RSS spreading with one listener per queue.
- **Neighbours and routes from the kernel**: read through netlink at build
  and on change; the user-space stack does not run ARP/NDP itself for
  addresses the kernel owns.
- **TLS**: `longshore_tls::record` seals into transmit frames and opens in
  place, as in the kernel transport, so every TLS provider works.
- **User-space TCP: decided by a spike.** Candidates: smoltcp, extended
  (window scaling, SACK, large windows, DCTCP/CUBIC, batched transmission),
  or a stack written for this. Criteria: interoperability with Linux TCP
  under loss, GB/s per core and p99 latency in the packet simulator and on
  the NICs, and the size of the changes needed. A dependency is adopted
  only with a benchmark in `bench/results/` (principle 7).
- **zcrx in the same study**: io_uring zero-copy receive keeps kernel TCP
  and removes the receive copy; if it gets close to AF_XDP on these NICs,
  it becomes an option of the kernel transport on kernels that have it.

## Risks / Trade-offs
- A user-space TCP stack is a large surface → interoperability tests
  against Linux peers with loss and reordering, the packet simulator from
  change 2, and fuzzing of the packet parser.
- Driver support for AF_XDP zero copy and multi-buffer varies → a support
  matrix in `docs/tcp.md`, and refusal with the missing piece named.
- Privileges (`CAP_NET_ADMIN`, `CAP_BPF`, `CAP_NET_RAW`) and exclusive NIC
  queues are deployment constraints → documented, checked at build.

## Migration Plan
Opt-in per `Net` by choosing the transport; programs keep the kernel
transport unless they choose otherwise.

## Open Questions
- smoltcp extended, or a stack written in tree (settled by the spike).
- Shared or per-core UMEM; huge page size (2 MiB or 1 GiB).
- Whether zcrx makes AF_XDP unnecessary on some of the target NICs.
