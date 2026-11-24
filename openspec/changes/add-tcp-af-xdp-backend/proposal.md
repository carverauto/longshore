# Change: Kernel bypass for longshore_tcp through AF_XDP, tuned for 200 and 300 Gb/s

## Why
At 200 and 300 Gb/s a NIC delivers 25 to 37.5 GB/s. Kernel TCP spends each
core on the receive copy (`copy_to_user` was 58% of the client on an HTTPS
download, milestone 28), softirq and interrupt work, and system-call
boundaries, so line rate needs many cores. AF_XDP hands NIC queues' frames
straight to user memory (UMEM) with zero copy; a TCP stack on the core that
owns the queue then delivers payload in place. These NICs are benchmarked
routinely, so this backend is held to line-rate numbers, not a proof of
concept.

Third of three changes (`add-tcp-crate`, `add-tcp-simulation-and-
benchmarks`, this). The API from change 1 does not change.

## What Changes
- `XdpTransport` behind the `Transport` seam: one AF_XDP socket per NIC
  queue per core; UMEM on huge pages, NUMA-local to the NIC, registered at
  build; fill, completion, receive and transmit rings; an XDP program that
  steers the program's flows to its sockets and passes all other traffic
  to the kernel.
- A user-space TCP stack fit for 200 Gb/s: window scaling, SACK,
  timestamps, windows of several megabytes, a data-centre congestion
  control (DCTCP with ECN, and CUBIC), batched transmission, jumbo frames
  through AF_XDP multi-buffer, checksum offload where the driver has it.
  Whether it starts from smoltcp or is written in tree is decided by a
  measured spike (principle 7).
- Flow placement: each connection's packets arrive on its owning core's
  queue (source ports chosen against the NIC's RSS hash, or flow rules);
  neighbour (ARP/NDP) and route data read from the kernel.
- TLS through the `longshore_tls` record layer, which does no I/O.
- io_uring zero-copy receive (`zcrx`, Linux 6.15+) evaluated in the same
  benchmarks as an option for kernel TCP.
- Tuning on physical hosts with 200 and 300 Gb/s NICs: queues, ring and
  batch sizes, busy polling, interrupt placement, huge pages, NUMA, core
  pinning; profiles and results in `bench/results/`.
- Refusal, not fallback: missing driver support, privileges or kernel
  features make building the transport fail with an error naming what is
  missing. Copy mode runs only when asked for.

## Impact
- Affected specs: `tcp-kernel-bypass` (ADDED)
- Affected code: `longshore_tcp` (`xdp` transport and user-space TCP),
  `longshore_common` (AF_XDP and XDP program loading on Linux),
  `longshore-bench`, `docs/tcp.md`
- Kernel: the io_uring backend keeps Linux 6.1. This backend states its own
  floor from the features it uses (AF_XDP multi-buffer needs 6.6) and
  refuses below it.
- Depends on `add-tcp-crate` and `add-tcp-simulation-and-benchmarks`
  (packet-level simulator and baselines)
