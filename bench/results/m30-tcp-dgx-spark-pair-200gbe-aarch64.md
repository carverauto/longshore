# TCP connection set: two DGX Sparks over 200 GbE

`tcp_uring rr`, `bulk` and `path` (see
`crates/longshore-bench/src/bin/tcp_uring/bench.rs`). Raw runs:
`m30-tcp-dgx-spark-pair-200gbe-aarch64.jsonl`; `where` says `loopback` or
`wire`, and a wire run carries the receiving end's figures under `peer`.

**Hosts:** two DGX Sparks, kernel 7.0.0-1019-nvidia, AArch64 (ten Cortex-X925
and ten Cortex-A725 cores), ConnectX-7 with `mlx5_core`, firmware 28.45.4028,
one 200 Gb/s link between them, MTU 9000, `rx-gro-hw` on. Both are working
Kubernetes nodes at low load (about 0.3 of 20 cores), with a system-call
monitor (`falco`) and a container network (`calico`) running.

**Loopback runs:** `scripts/bench-tcp.sh --secs 3 --runs 3` on one of them,
threads not pinned. **Wire runs:** the peer (`--listen`) on one machine, the
load (`--connect`) on the other, one thread each, each pinned to one
Cortex-X925 core, 5 seconds, one run each. Card queues, interrupt placement
and coalescing were left as found.

**What this can tell:** what one thread sends and receives over a real card
with kernel TCP, and what that costs in CPU. **What it cannot:** the limit of
the link (one thread a side never came near 200 Gb/s), or anything precise:
the wire runs were made once each on machines that do other work.

## What it shows

- **One receiving thread is full at about 50 Gb/s.** 6.1 to 6.7 GB/s on the
  ring with the receiving thread at 88 to 100 % of its core, 0.13 to
  0.16 CPU-seconds a GB. On epoll 4.4 to 5.0 GB/s at 0.20 to 0.23.
  Direct receive of 1 MiB frames is the cheapest: 6.7 GB/s at 0.130.
- **Sending is about six times cheaper than receiving.** With zero-copy
  sends of 1 MiB the sending thread spends 0.024 to 0.030 CPU-seconds a GB
  (15 to 20 % of a core at 6.5 GB/s) while the receiver is full. Copied
  sends cost 0.083 (64 KiB) to 0.186 (1 MiB) on the ring.
- **Zero-copy sends of one 64 KiB segment each are slow on a real card.**
  0.7 to 1.2 GB/s at 1 to 4 connections with both ends mostly idle
  (receiver 21 to 34 %, sender 7 to 10 % of a core); 3.7 GB/s at 16
  connections. The same frames with zero-copy off: 6.1 GB/s. With sixteen
  of them in one 1 MiB segment: 6.2 GB/s, the sender at 16 %. With 256 KiB
  frames and segments: 3.4 GB/s. Spinning does not help (0.95 GB/s), so the
  wait is not the thread's. The reading: a connection has sixteen sends in
  flight at most here, and a zero-copy send is done only when its
  notification comes, which over a card is milliseconds later; throughput
  is then sixteen segments a notification delay. Not traced into the
  kernel. Loopback cannot show this: it copies and notifies at once.
- **Request and response over the wire.** 324 bytes, ring: 47,000 a second
  on one connection (p50 20 µs, p99 26 µs), 220,000 on 16, 310,000 on 256
  (p99 1.1 ms). Epoll: 31,000, 154,000, 173,000. Spinning made one
  connection worse here (25,000 a second, p99 524 µs), not explained.
- **Loopback on this machine is slower than in the laptop's container**
  (bulk 4 to 5.5 GB/s against 11.5 to 14.7; request and response 0.27
  million a second at 256 connections against 1.0 million). Threads were not
  pinned and the machine has slow cores and a system-call monitor; which of
  these it is was not measured. The wire runs, pinned, are the ones to read.

## What it means for kernel bypass

Receive is where the CPU goes: one core takes a quarter of the link, and
sending the same bytes costs a sixth of it. Both ways around the kernel's
receive copy are in this kernel (`CONFIG_XDP_SOCKETS`,
`CONFIG_IO_URING_ZCRX`), and the card's header and data split
(`rx-gro-hw`) can be turned on. Neither was measured: longshore has no
code for either yet.

## Follow-ups

- Zero-copy sends: either raise the default segment for bulk senders, or
  let a connection have more zero-copy sends in flight, and find where the
  notification waits.
- Repeat the wire runs with several receiving threads and with interrupts
  placed, to reach the link's limit.
