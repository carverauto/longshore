# Milestone 34: the three receive modes on one interface of a DGX Spark's port

Task 5.1 of `add-tcp-zero-copy-receive`, measured 2026-11-27 between the
two DGX Sparks of milestones 30 to 32: `donbot` receives, `roberto` sends,
over the second of the port's two interfaces only (`enP2p1s0f0np0`, about
110 Gb/s, milestone 30; the first interface carries the cluster and was
left alone). Receiver: Linux 7.0.0-1019-nvidia, AArch64, ConnectX-7
firmware 28.45.4028, header and data split on, `rx-gro-hw` on, receive
ring of 1,024 entries. Raw lines: `m34-tcp-receive-modes-dgx-spark.jsonl`,
one a run, not in the order run (the spare sizes ran in the order 0,
1,024, 4,096); the scripts in `bench/spikes/zcrx/group5/`.

Reviewed independently against the raw lines (task 5.4); this text is the
corrected one. What could not be corrected from the data is said where it
matters.

## How it was measured

- Receiver `k` of a cell listens on port 9311+k; a flow rule steers that
  port to queue 15+k, whose interrupts run on core 15+k; the receiving
  thread runs on core 5+k (Cortex-X925 cores both). Other traffic is
  spread over queues 0 to 14. Every mode uses this layout.
- `tcp_uring bulk --listen` on `donbot`, `--rx ring|direct|zc`, two
  connections a receiver, sized for the cell's frame; `tcp_uring bulk
  --connect` on `roberto`, one sender a receiver on cores 5 to 7, eight
  seconds, `--frame-on-packet on` unless said. That option sends one
  frame at a time, the next when the transport has taken the last, with
  `TCP_NODELAY`: the small-frame cells measure that sender as much as the
  receiver. The senders also send with zero copy (four in flight, from
  16 KiB). The senders' own lines were kept only for the cells of the
  last session (the spare sizes and request-answer).
- Thread CPU-s a GB: the receiving threads' own CPU time over their bytes,
  both from the first frame to the end.
- Queues' cores CPU-s a GB: busy time of cores 15+k from `/proc/stat`
  over the whole cell (about 11 s, three of them before the sender
  starts). The sum used counted iowait as busy (fixed in `cell.sh` since):
  no receiver sleeps on those cores, so it should add little there, but
  that is not shown. The same sum on the threads' cores measured how long
  each receiver lived, not what it spent, and is left out.
- Drops: `rx_out_of_buffer`, a counter of the whole interface.
- Zero-copy with 64 MiB of spare area (`--zc-spare 16384`) unless said.
- Receivers of the 324-byte and 64 KiB cells were built before two fixes
  of the review of group 3 (write holds; the ring of returns sized by
  pieces), which `bulk` does not reach; the commit of each session's
  build was not recorded.

## Every cell, three runs

| Kind | MTU | Frame | Mode | Cores | Sender's option | Spare | GB/s | Thread CPU-s a GB | Queues' cores CPU-s a GB | Kept | Drops |
|---|---|---|---|---|---|---|---|---|---|---|---|
| bulk | 9000 | 324 B | Ring | 1 | on | 16384 | 0.12, 0.13, 0.11 | 7.489, 6.776, 7.646 | 8.157, 7.827, 8.288 | - | 205, 94, 88 |
| bulk | 9000 | 324 B | Ring | 3 | on | 16384 | 0.31, 0.32, 0.30 | 7.513, 7.294, 7.224 | 6.058, 5.913, 6.783 | - | 137, 139, 310 |
| bulk | 9000 | 324 B | Direct | 1 | on | 16384 | 0.10, 0.10, 0.12 | 9.669, 9.501, 8.620 | 10.217, 10.012, 8.674 | - | 53, 0, 0 |
| bulk | 9000 | 324 B | Direct | 3 | on | 16384 | 0.29, 0.30, 0.33 | 9.299, 9.117, 8.698 | 8.255, 8.882, 8.691 | - | 0, 139, 430 |
| bulk | 9000 | 324 B | ZeroCopy | 1 | on | 16384 | 0.11, 0.11, 0.10 | 8.816, 9.213, 10.198 | 8.839, 9.213, 10.539 | 0.000 to 0.000 % | 164, 0, 0 |
| bulk | 9000 | 324 B | ZeroCopy | 3 | on | 16384 | 0.28, 0.29, 0.24 | 10.426, 9.573, 9.979 | 10.739, 7.802, 5.650 | 0.000 to 0.000 % | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Ring | 1 | on | 16384 | 4.69, 4.68, 4.66 | 0.212, 0.213, 0.214 | 0.051, 0.055, 0.047 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Ring | 3 | on | 16384 | 13.05, 13.02, 13.02 | 0.228, 0.229, 0.229 | 0.036, 0.038, 0.042 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Direct | 1 | on | 16384 | 5.33, 5.23, 5.21 | 0.176, 0.176, 0.177 | 0.059, 0.053, 0.057 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Direct | 3 | on | 16384 | 13.73, 13.12, 13.57 | 0.194, 0.195, 0.195 | 0.052, 0.034, 0.038 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | ZeroCopy | 1 | on | 16384 | 4.92, 4.58, 4.60 | 0.201, 0.217, 0.216 | 0.053, 0.050, 0.050 | 0.008 to 0.015 % | 0, 0, 0 |
| bulk | 9000 | 64 KiB | ZeroCopy | 3 | on | 16384 | 13.25, 12.87, 13.40 | 0.225, 0.216, 0.223 | 0.046, 0.039, 0.041 | 0.006 to 0.011 % | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Ring | 1 | on | 16384 | 6.36, 6.48, 6.33 | 0.157, 0.154, 0.158 | 0.036, 0.047, 0.040 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Ring | 3 | on | 16384 | 13.85, 13.85, 13.83 | 0.197, 0.197, 0.199 | 0.039, 0.037, 0.042 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Direct | 1 | on | 16384 | 7.43, 7.43, 7.39 | 0.134, 0.134, 0.135 | 0.043, 0.042, 0.043 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Direct | 3 | on | 16384 | 13.84, 13.85, 13.84 | 0.171, 0.171, 0.171 | 0.035, 0.025, 0.026 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | ZeroCopy | 1 | on | 16384 | 5.66, 5.59, 5.61 | 0.177, 0.179, 0.178 | 0.033, 0.027, 0.027 | 0.000 to 0.000 % | 0, 0, 0 |
| bulk | 9000 | 1 MiB | ZeroCopy | 3 | on | 16384 | 13.85, 13.80, 13.85 | 0.200, 0.203, 0.201 | 0.024, 0.036, 0.043 | 0.000 to 0.000 % | 0, 0, 0 |
| bulk | 8244 | 324 B | Ring | 1 | on | 16384 | 0.12, 0.13, 0.13 | 7.177, 6.876, 6.732 | 8.653, 7.927, 7.731 | - | 165, 187, 200 |
| bulk | 8244 | 324 B | Ring | 3 | on | 16384 | 0.28, 0.36, 0.34 | 6.890, 7.016, 7.284 | 4.879, 8.146, 5.837 | - | 44, 204, 159 |
| bulk | 8244 | 324 B | Direct | 1 | on | 16384 | 0.10, 0.11, 0.10 | 9.303, 8.968, 9.279 | 9.627, 9.209, 9.639 | - | 30, 93, 0 |
| bulk | 8244 | 324 B | Direct | 3 | on | 16384 | 0.30, 0.28, 0.32 | 9.285, 9.189, 8.842 | 8.524, 6.486, 7.636 | - | 89, 0, 300 |
| bulk | 8244 | 324 B | ZeroCopy | 1 | on | 16384 | 0.10, 0.09, 0.10 | 9.636, 10.605, 10.013 | 9.527, 11.167, 10.217 | 0.000 to 0.000 % | 94, 124, 0 |
| bulk | 8244 | 324 B | ZeroCopy | 3 | on | 16384 | 0.31, 0.25, 0.30 | 9.410, 9.493, 9.916 | 8.969, 5.489, 9.975 | 0.000 to 0.000 % | 97, 0, 159 |
| bulk | 8244 | 64 KiB | Ring | 1 | on | 16384 | 3.48, 4.39, 3.75 | 0.212, 0.211, 0.212 | 0.050, 0.061, 0.054 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | Ring | 3 | on | 16384 | 12.42, 12.55, 12.86 | 0.226, 0.225, 0.224 | 0.041, 0.047, 0.033 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | Direct | 1 | on | 16384 | 4.19, 4.76, 4.15 | 0.174, 0.174, 0.174 | 0.064, 0.084, 0.061 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | Direct | 3 | on | 16384 | 13.04, 12.68, 13.09 | 0.188, 0.189, 0.189 | 0.050, 0.047, 0.047 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 16384 | 5.76, 5.71, 5.55 | 0.125, 0.125, 0.124 | 0.087, 0.088, 0.085 | 99.996 to 99.997 % | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 3 | on | 16384 | 13.02, 13.40, 13.11 | 0.131, 0.131, 0.132 | 0.060, 0.052, 0.055 | 99.970 to 99.996 % | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Ring | 1 | on | 16384 | 3.61, 2.54, 3.09 | 0.177, 0.187, 0.182 | 0.061, 0.059, 0.050 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Ring | 3 | on | 16384 | 9.73, 10.48, 9.73 | 0.199, 0.198, 0.199 | 0.043, 0.047, 0.050 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Direct | 1 | on | 16384 | 3.01, 3.10, 2.19 | 0.150, 0.149, 0.156 | 0.054, 0.045, 0.052 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Direct | 3 | on | 16384 | 9.21, 11.29, 10.55 | 0.168, 0.165, 0.168 | 0.046, 0.042, 0.044 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | ZeroCopy | 1 | on | 16384 | 6.57, 3.04, 2.20 | 0.076, 0.088, 0.093 | 0.051, 0.060, 0.045 | 99.976 to 99.986 % | 0, 0, 0 |
| bulk | 8244 | 1 MiB | ZeroCopy | 3 | on | 16384 | 10.52, 10.16, 12.05 | 0.090, 0.087, 0.086 | 0.050, 0.051, 0.048 | 99.906 to 99.987 % | 0, 0, 0 |

## What it shows

- **Frames of whole pages at MTU 8244 are kept in their pages.** With the
  sender's option, 99.91 to 99.997 % of 64 KiB and 1 MiB frames were
  kept (4 to 483 moved a run). At MTU 9000 almost none were (64 KiB: 17
  to 90 frames a receiver, at most 0.015 %).
- **Kept frames cost the receiving thread less.** One core, MTU 8244:
  64 KiB frames 0.124 to 0.125 CPU-s a GB in zero-copy against 0.174
  direct and 0.211 to 0.212 ring; 1 MiB frames 0.076 to 0.093 against
  0.149 to 0.156 direct and 0.177 to 0.187 ring. The queues' cores add
  about 0.05 to 0.09 in every mode. Milestone 31's single connection
  found zero-copy at about half direct's cost (0.108 to 0.113 against
  0.208 to 0.210); here it is about 0.7 of it, with two connections a
  receiver and a different queue and build: the gap is real, its size
  depends on the setup.
- **The rates at MTU 8244 are not the modes' capacity.** There the
  receiving threads were not saturated (zero-copy 69 to 72 % busy, direct
  72 to 83 %, ring 74 to 93 % at 64 KiB; 20 to 69 % at 1 MiB), and ring
  and direct mode were themselves slower and more variable than at MTU
  9000, where their threads were 92 to 100 % busy. Something at MTU 8244
  slows every mode, most of all with 1 MiB frames (2.2 to 6.6 GB/s on one
  core); the senders' lines that could say whether it is the sender were
  not kept. Compare the modes by CPU a GB, not by GB/s, here.
- **Frames that do not line up cost zero-copy more.** At MTU 9000 with
  1 MiB frames, zero-copy moved every frame and carried 5.6 GB/s at 0.18
  CPU-s a GB against direct mode's 7.4 at 0.134. Zero-copy is for traffic
  whose packets carry whole pages.
- **The interface's limit, about 13.85 GB/s, was reached only with 1 MiB
  frames at MTU 9000**, in every mode on three cores. With 64 KiB frames
  three cores gave 12.4 to 13.7 GB/s, and with 1 MiB at MTU 8244 9.2 to
  12.1.
- **Small frames.** At 324 bytes one core took 282,000 to 403,000 frames
  a second (ring 342,000 to 403,000, direct 302,000 to 356,000, zero-copy
  282,000 to 349,000; thread CPU 6.7 to 7.6, 8.6 to 9.7 and 8.8 to 10.6
  CPU-s a GB), and three cores 2.6 to 3.0 times that. The queue's core was
  busy 7.3 to 8.0 s of the eight, and the card dropped packets in 24 of
  36 runs (up to 430): the limit may be the kernel's work a packet on the
  queue's core, or the sender's one frame at a time, as much as the
  receive. No profile was taken. Zero-copy keeps nothing at this size (a
  frame below `zc_min_frame` is moved by design) and costs the most.

## The area, and the sender's option

| Kind | MTU | Frame | Mode | Cores | Sender's option | Spare | GB/s | Thread CPU-s a GB | Queues' cores CPU-s a GB | Kept | Drops |
|---|---|---|---|---|---|---|---|---|---|---|---|
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | off | 16384 | 3.68, 3.15, 3.38 | 0.118, 0.120, 0.120 | 0.079, 0.077, 0.075 | 99.186 to 99.912 % | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 4096 | 5.85, 5.05, 5.52 | 0.125, 0.127, 0.124 | 0.092, 0.088, 0.112 | 99.972 to 99.995 % | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 1024 | 1.28, 0.00, 0.00 | 0.133, 0.000, 0.000 | 0.117, 0.000, 0.000 | 0.000 to 99.958 % | 5559, 3522, 2137 | errors: ['tcp_uring: the load did not finish: 0 of 2 connections came and closed']
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 0 | 0.00, 0.00, 0.00 | 0.000, 0.000, 0.000 | 0.000, 0.000, 0.000 | 0.000 to 0.000 % | 0, 0, 0 | errors: ['tcp_uring: the load did not finish: 0 of 2 connections came and closed']
| bulk | 8244 | 64 KiB | ZeroCopy | 3 | on | 0 | 0.00, 0.00, 0.00 | 0.000, 0.000, 0.000 | 0.000, 0.000, 0.000 | 0.000 to 0.000 % | 0, 0, 0 | errors: ['tcp_uring: the load did not finish: 0 of 2 connections came and closed']

- **The area.** The ring has 1,024 entries and a packet at MTU 8244 fills
  two pages, so the card's own share is about 2,048 pages. With no spare
  (about 200 pages in all) the receivers saw no frame in the 100 s they
  waited, and `ethtool -S` showed `rx15_buff_alloc_err` 24 afterwards
  (read once, not recorded per run). With 1,024 spare pages (about 1,224
  in all) one run carried 1.28 GB/s while the card dropped 5,559 packets,
  and in the two after it the senders got 0.76 and 0.55 GB/s through
  with 3,522 and 2,137 drops, and no connection ended in time. With 4,096
  spare pages the cell ran as with 16,384, with no drop. So between
  about 1,200 and 4,300 pages the area goes from starved to enough; the
  2,048 that the card's share predicts was not tried, nor MTU 9000.
- **The sender's option.** Off, 99.19 to 99.91 % of 64 KiB frames were
  kept, at 3.1 to 3.7 GB/s against 5.6 to 5.8 with it; the receiving
  threads were 39 to 45 % busy, so the slower rate is not the receive's,
  and why is not known (no sender lines). Milestone 31, with one
  connection, kept 67 and 85 % without it. With it, 99.9 % or more in
  every run here.

## Request and answer, `tcp_uring rr`

324-byte requests (`rr` takes no `--frame`; the raw lines say 65536),
two connections, each answered after `hold`; MTU 8244, one core, three
runs; a closed loop, so round trips a second say latency, not capacity
(the receiving thread was 32 to 41 % busy). Percentiles fall in the
histogram's buckets, about 6 % apart.

| Mode | Round trips a second | p50 µs | p99 µs | p99.9 µs |
|---|---|---|---|---|
| Ring | 52,638, 53,587, 54,847 | 32.8, 31.7, 32.8 | 164, 172, 148 | 623, 590, 492 |
| Direct | 52,234, 55,248, 52,211 | 32.8, 30.7, 34.8 | 164, 148, 156 | 524, 623, 557 |
| ZeroCopy | 44,056, 52,727, 42,278 | 34.8, 34.8, 34.8 | 410, 57, 442 | 852, 410, 819 |

Every request was moved in zero-copy, so no answer came from a frame in
pages. In two runs of three zero-copy had fewer round trips and a longer
tail.

## Not measured

- Both interfaces, and any mode but direct at the port's full 200 Gb/s
  (milestone 30, the kernel's copy, one run a row).
- Logging in ring and direct mode; the log from pages is `zc_uring log`'s
  (milestone 31: 5.4 GB/s with 4 MiB writes at 0.105 CPU-s a GB, one
  run).
- Answers sent from frames in pages; frames checked over every byte
  (`bulk` reads one byte); the share kept without the sender's option at
  other sizes, MTUs and core counts.
- A profile of the small-frame cells, and the senders' side of every cell
  but the last session's.
