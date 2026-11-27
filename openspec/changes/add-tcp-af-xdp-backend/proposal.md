# Change: Kernel bypass for longshore_tcp through AF_XDP, tuned for 200 and 300 Gb/s

## Why
At 200 and 300 Gb/s a NIC delivers 25 to 37.5 GB/s. Kernel TCP spends each
core on the receive copy (`copy_to_user` was 58% of the client on an HTTPS
download, milestone 28), softirq and interrupt work, and system-call
boundaries, so line rate needs many cores. AF_XDP hands NIC queues' packets
straight to user memory (UMEM) without the kernel's stack; a TCP stack on
the core that owns the queue then moves payload once into frame memory and
transmits from it in place. These NICs are benchmarked
routinely, so this backend is held to line-rate numbers, not a proof of
concept.

Third of three changes (`add-tcp-crate`, `add-tcp-simulation-and-
benchmarks`, this). The API from change 1 does not change.

**Held behind `add-tcp-zero-copy-receive`.** Measured between two DGX
Sparks (milestone 30), kernel TCP fills a 200 Gb/s link with four to eight
receiving cores, and the receive copy is what those cores do. io_uring
zero-copy receive removes that copy and keeps the kernel's TCP, so it is
built and measured first. This change starts only if those numbers leave
something for a user-space stack to win, and its case is then restated
from them (task 0.0).

**The case, restated from `add-tcp-zero-copy-receive`** (milestones 31
and 34, one interface of a Spark's port, about 110 Gb/s; milestone 34 as
corrected by its review):

- *Large frames: no case.* With zero-copy receive the kernel's TCP keeps
  99.9 % of frames of whole pages in the area, and the receiving thread
  spends 0.124 to 0.125 CPU-s a GB on 64 KiB frames and 0.076 to 0.093
  on 1 MiB, against direct mode's 0.174 and 0.149 to 0.156. This change's
  own design moves every payload once from UMEM into frame memory, the
  cost of a move like direct mode's: on large frames it would be dearer
  than zero-copy receive, not cheaper.
- *Small frames: not shown either way.* 324-byte frames came to 282,000
  to 403,000 a second a core, with the queue's core nearly busy, the
  card dropping packets, and every sender sending one frame at a time
  (`with_frame_on_packet`); request and answer took a median of 31 to
  35 µs. Where the cost of a small frame lies (the sender's pacing, the
  kernel's work a packet, system calls, the set's own work a frame) was
  not measured; only the last two are what a user-space stack removes,
  and the set's own it can cut in place.

So the measurements give this change no case yet. Before go or close: the
324-byte cells again without the sender's option, with the senders' lines
kept, and a profile of the receiving and the queue's cores (task 0.0).

## What Changes
- `XdpTransport` behind the `Transport` seam, its `Driver` the core's
  AF_XDP sockets: one socket per NIC queue per core; fill, completion,
  receive and transmit rings; an XDP program that
  steers the program's flows to its sockets and passes all other traffic
  to the kernel.
- One pool for both worlds: the `Io` buffer pool on huge pages,
  NUMA-local to the NIC, laid out in 4 KiB chunks and registered both as
  io_uring buffers and as the UMEM. Extents from change 1 are then
  transmitted in place (packet headers in one fragment, payload fragments
  pointing into the extent), and files and the S3 client keep running on
  the thread's ring from the same memory.
- One move on receive, as with kernel TCP: packets arrive in UMEM chunks
  after their headers, which no direct-I/O write can take, so the
  transport moves payload once into its extent, and the caller may fuse a
  pass of its own (a checksum) into that move.
- Waiting: `Op::PollFd`, a multishot poll on an outside descriptor, lets
  the thread sleep in its one ring with the AF_XDP socket armed there when
  idle, and spin on the NIC queue under load; the `Turn` helper asks each
  source whether the thread may sleep. The same rule serves a polled NVMe
  ring, which only needs attention while it has operations in flight.
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
- Tuning on physical hosts with 200 and 300 Gb/s NICs: queues, ring and
  batch sizes, busy polling, interrupt placement, huge pages, NUMA, core
  pinning; profiles and results in `bench/results/`.
- Built to a stated contract, after what the review of `add-tcp-crate`
  showed: `design.md` holds the state table of the TCP connection, the
  ownership table of UMEM chunks, the state table of a queue's socket,
  eleven invariants and an answer for every empty pool. They are reviewed
  before any code; a packet-level model test checks them from the first
  commit, and the transport passes change 2's conformance suite. `close`
  frees its slot at once and leaves the rest of the close handshake to a
  fixed linger table, so a slot's next connection never sees its
  predecessor's packets.
- Refusal, not fallback: missing driver support, privileges or kernel
  features make building the transport fail with an error naming what is
  missing. AF_XDP's copy mode (the driver copying packets between the NIC
  and the UMEM) runs only when asked for.

## Impact
- Affected specs: `tcp-kernel-bypass` (ADDED)
- Affected code: `longshore_tcp` (`xdp` transport and user-space TCP),
  `longshore_common` (AF_XDP and XDP program loading on Linux),
  `longshore-bench`, `docs/tcp.md`
- Kernel: the io_uring backend keeps Linux 6.1. This backend states its own
  floor from the features it uses (AF_XDP multi-buffer needs 6.6) and
  refuses below it.
- Depends on `add-tcp-crate` and `add-tcp-simulation-and-benchmarks`
  (packet-level simulator and baselines), and on the results of
  `add-tcp-zero-copy-receive`
