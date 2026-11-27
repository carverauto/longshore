# Milestone 35: where a small frame's cost goes, and what kernel bypass could win

Task 0.0 of `add-tcp-af-xdp-backend` asked whether small frames leave a
case for a user-space TCP stack; milestone 34 could not say, since every
sender there sent one frame at a time. Measured 2026-11-27 on the same
pair: `donbot` receives on core 9 of its second interface's queue 19
(interrupts on core 19, MTU 9000), `roberto` sends. Raw lines:
`m35-tcp-small-frames-kernel-share-dgx-spark.jsonl`, the profiles in
`m35-profiles/`; scripts
`bench/spikes/zcrx/group5/small.sh` and `rr.sh`.

## How

- One receiver, `tcp_uring bulk --listen --rx ring|direct --conns 12`,
  on core 9; three senders on cores 5 to 7 of `roberto`, four
  connections each, 324-byte frames for eight seconds; three runs a
  configuration.
- Cores 9 and 19 busy over four seconds in the middle of each run, from
  `/proc/stat` (user, nice, system, irq, softirq, steal; not iowait).
- In the first run of each configuration, `perf record -C 9,19 -F 4999`
  for those four seconds (the reports, `perf report --sort cpu,dso,sym`); samples grouped by function into the set's own
  code (`tcp_uring`), the kernel's copy, its network stack, netfilter,
  io_uring, the driver and the rest.
- Request and answer: `tcp_uring rr`, two connections, ring mode, the
  answering side on core 9, with `--wait sleep` and with `--wait spin` on
  both ends, three runs each.

The receiver of the small-frame runs was started without `--frame 324`
(a slip in `small.sh` then, fixed since), so it was sized for the default
64 KiB frames: its eight entries in the JSONL record `"frame":65536`,
while every sender's records `"frame":324`. The `rr` runs are not
affected: both ends record 324. What it changed is the receiver's frame
memory alone: 24 slots of 64 KiB rather than of 4 KiB, and the largest
frame it would take. The frames on the wire, and so the packets, the
bytes and the counts, are the senders' 324-byte frames, and the
receiver's frame count is within 0.04 % of theirs. In ring mode a frame is read
where it lies in a 4 KiB ring buffer, and only one split across two is
put together in frame memory; in direct mode every frame is received into
frame memory, so there the 324 bytes of each frame were spread over
larger slots, more pages for the same bytes. Whether that cost direct
mode anything was not measured; the runs were not repeated.

## Small frames, three shapes of traffic

| Senders | Mode | Frames a second (millions) | Core 9 | Core 19 (queue) |
|---|---|---|---|---|
| batching (`--frame-on-packet off`) | Ring | 11.38, 11.37, 11.29 | fully busy: the set 36 to 38 %, the kernel 61 to 63 % | 6 to 14 % busy |
| batching | Direct | 1.98, 2.01, 2.00 | fully busy: the set 23 to 33 %, the kernel 66 to 76 % | 2 to 7 % |
| one frame a packet (`--frame-on-packet on`) | Ring | 0.39, 0.40 | 73 to 92 % busy, the set 3 to 4 % | 78 to 99 %, softirq |

The receiving thread was fully busy in every batching run (8.0 CPU-s in
8 s), and the senders' and the receiver's counts agree: the receiver was
the limit. One frame a packet was run twice, for contrast.

What the profiles give, share of each core's samples:

| Traffic | Core | The set's own code | Kernel copy | Kernel network stack and the rest | Netfilter |
|---|---|---|---|---|---|
| batching, Ring | 9 | 38 % | 32 % | about 27 % | 3 % |
| batching, Ring | 19 | - | 4 % | about 78 % | 18 % |
| batching, Direct | 9 | 31 % | 7 % | about 60 % (io_uring 16 %) | 2 % |
| one frame a packet, Ring | 9 | 4 % | 3 % | about 59 % | 34 % |
| one frame a packet, Ring | 19 | - | 1 % | about 61 % | 38 % |

Netfilter is the Sparks' Kubernetes node firewall: connection tracking,
NAT and about 2,700 nftables rules (`nft list ruleset`), applied to the
test traffic as to any other. A server without them, or one that exempts
its data ports from connection tracking, does not pay it.

## Request and answer

| Both ends | Round trips a second | p50 µs | p99 µs | p99.9 µs |
|---|---|---|---|---|
| sleeping | 62,714, 58,463, 54,943 | 27.6, 29.7, 32.8 | 131, 94, 55 | 524, 557, 410 |
| spinning | 70,788, 68,081, 65,196 | 24.6, 27.6, 26.6 | 61, 41, 43 | 442, 51, 655 |

Spinning takes 3 to 5 µs off the median and most of the tail at p99:
the wait is a small part of a round trip.

## What it settles

- **Small frames in a stream: no case for bypass.** When the senders
  batch, the kernel packs many frames into a packet and one core takes
  11.3 million 324-byte frames a second. The kernel's share of that core
  is about 62 %, half of it the copy; a bypass stack would keep a move
  of its own (its design moves every payload once) and add a TCP stack:
  even removing every other kernel cycle, the ceiling is about 1.3 to
  1.4 times. Direct mode is the wrong mode here (2.0 million: a receive
  for each header and body); ring mode is the right one.
- **One small frame a packet: the one case.** With every frame its own
  packet, the kernel's per-packet path saturates both the receiving core
  and the queue's core at about 400,000 frames a second, and the set's
  own code is 3 to 4 % of the receiving core. That is what a user-space
  stack replaces. A third of it here is the node's firewall, which a
  server can drop without bypass; the rest, about 3 µs of CPU a packet
  over the two cores, remains.
- **Latency.** A round trip of 25 to 33 µs median is mostly the two
  hosts' per-packet paths, not the wait: the same path, and the same
  case.

So bypass pays only for servers that must take independent small
requests at well over about 400,000 a second a core pair, or answer them
in well under about 25 µs: the traffic of trading systems and exchanges
sending order messages, not of a database, whose query results and
inserts cross many frames. On that ground `add-tcp-af-xdp-backend` was
closed (2026-11-27). Before it, and cheaper: exempting the data
ports from connection tracking (a third of the per-packet cost here),
and spinning while the thread is loaded (3 to 5 µs a round trip).

## Not measured

- The per-packet cost with the firewall exempted: it needs a rule on a
  cluster node's firewall, which was not changed.
- A user-space stack itself: what it would spend a packet is not
  measured here.
- More than one receiving core, and frame sizes between 324 bytes and
  64 KiB.
