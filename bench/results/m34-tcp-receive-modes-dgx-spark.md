# Milestone 34: the three receive modes on a DGX Spark's 200 GbE port

Task 5.1 of `add-tcp-zero-copy-receive`, measured 2026-11-27 between
the two DGX Sparks of milestones 30 to 32: `donbot` receives, `roberto`
sends, over the second of the port's two interfaces only
(`enP2p1s0f0np0`, behind a PCIe 5.0 x4 link, about 110 Gb/s; the first
interface carries the cluster and was left alone). Linux
7.0.0-1019-nvidia, AArch64, ConnectX-7 firmware 28.45.4028, header and
data split on, `rx-gro-hw` on. Raw lines: `m34-tcp-receive-modes-dgx-spark.jsonl`,
one a run; the scripts in `bench/spikes/zcrx/group5/`.

## How it was measured

- Receiver `k` of a cell listens on port 9311+k; a flow rule steers that
  port to queue 15+k, whose interrupts run on core 15+k; the receiving
  thread runs on core 5+k (both sets are Cortex-X925 cores). Other
  traffic is spread over queues 0 to 14. Every mode uses this layout.
- `tcp_uring bulk --listen` on `donbot`, `--rx ring|direct|zc`, two
  connections a receiver; `tcp_uring bulk --connect` on `roberto`, one
  sender a receiver, on cores 5 to 7, for eight seconds, with
  `--frame-on-packet on` unless said.
- Thread CPU-s a GB: the receiving threads' own CPU time. Threads' cores
  and queues' cores: busy time of cores 5+k and 15+k from `/proc/stat`
  over the cell, whatever ran there: the threads' cores show about twice
  the threads' own time in every mode, so they carry other work (the
  node is a cluster member); the queues' cores are the kernel's packet
  work for the cell's queues.
- Drops: `rx_out_of_buffer` over the cell.
- Zero-copy with 64 MiB of spare area (`--zc-spare 16384`) unless said.
- The receivers of the 324-byte and 64 KiB cells were built before two
  fixes of the review of group 3 (write holds; the ring of returns
  sized by pieces), which `bulk` does not reach. The first run of the
  1 MiB cells was lost to the cell script sizing receivers for 64 KiB;
  they were run again, sized for the frame.

## Every cell, three runs

| Kind | MTU | Frame | Mode | Cores | Sender's option | Spare | GB/s | Thread CPU-s a GB | Threads' cores | Queues' cores | Kept | Drops |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| bulk | 9000 | 324 B | Ring | 1 | on | 16384 | 0.12, 0.13, 0.11 | 7.489, 6.776, 7.646 | 11.547, 10.685, 12.128 | 8.157, 7.827, 8.288 | - | 205, 94, 88 |
| bulk | 9000 | 324 B | Ring | 3 | on | 16384 | 0.31, 0.32, 0.30 | 7.513, 7.294, 7.224 | 13.231, 12.897, 13.659 | 6.058, 5.913, 6.783 | - | 137, 139, 310 |
| bulk | 9000 | 324 B | Direct | 1 | on | 16384 | 0.10, 0.10, 0.12 | 9.669, 9.501, 8.620 | 14.000, 13.708, 12.033 | 10.217, 10.012, 8.674 | - | 53, 0, 0 |
| bulk | 9000 | 324 B | Direct | 3 | on | 16384 | 0.29, 0.30, 0.33 | 9.299, 9.117, 8.698 | 14.119, 13.576, 12.314 | 8.255, 8.882, 8.691 | - | 0, 139, 430 |
| bulk | 9000 | 324 B | ZeroCopy | 1 | on | 16384 | 0.11, 0.11, 0.10 | 8.816, 9.213, 10.198 | 11.479, 11.918, 13.496 | 8.839, 9.213, 10.539 | 0 to 0 % | 164, 0, 0 |
| bulk | 9000 | 324 B | ZeroCopy | 3 | on | 16384 | 0.28, 0.29, 0.24 | 10.426, 9.573, 9.979 | 12.918, 12.357, 15.112 | 10.739, 7.802, 5.650 | 0 to 0 % | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Ring | 1 | on | 16384 | 4.69, 4.68, 4.66 | 0.212, 0.213, 0.214 | 0.294, 0.294, 0.296 | 0.051, 0.055, 0.047 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Ring | 3 | on | 16384 | 13.05, 13.02, 13.02 | 0.228, 0.229, 0.229 | 0.317, 0.318, 0.317 | 0.036, 0.038, 0.042 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Direct | 1 | on | 16384 | 5.33, 5.23, 5.21 | 0.176, 0.176, 0.177 | 0.259, 0.264, 0.264 | 0.059, 0.053, 0.057 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | Direct | 3 | on | 16384 | 13.73, 13.12, 13.57 | 0.194, 0.195, 0.195 | 0.302, 0.317, 0.308 | 0.052, 0.034, 0.038 | - | 0, 0, 0 |
| bulk | 9000 | 64 KiB | ZeroCopy | 1 | on | 16384 | 4.92, 4.58, 4.60 | 0.201, 0.217, 0.216 | 0.265, 0.284, 0.280 | 0.053, 0.050, 0.050 | 0 to 0 % | 0, 0, 0 |
| bulk | 9000 | 64 KiB | ZeroCopy | 3 | on | 16384 | 13.25, 12.87, 13.40 | 0.225, 0.216, 0.223 | 0.275, 0.283, 0.270 | 0.046, 0.039, 0.041 | 0 to 0 % | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Ring | 1 | on | 16384 | 6.36, 6.48, 6.33 | 0.157, 0.154, 0.158 | 0.218, 0.214, 0.219 | 0.036, 0.047, 0.040 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Ring | 3 | on | 16384 | 13.85, 13.85, 13.83 | 0.197, 0.197, 0.199 | 0.301, 0.301, 0.300 | 0.039, 0.037, 0.042 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Direct | 1 | on | 16384 | 7.43, 7.43, 7.39 | 0.134, 0.134, 0.135 | 0.185, 0.185, 0.185 | 0.043, 0.042, 0.043 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | Direct | 3 | on | 16384 | 13.84, 13.85, 13.84 | 0.171, 0.171, 0.171 | 0.298, 0.302, 0.302 | 0.035, 0.025, 0.026 | - | 0, 0, 0 |
| bulk | 9000 | 1 MiB | ZeroCopy | 1 | on | 16384 | 5.66, 5.59, 5.61 | 0.177, 0.179, 0.178 | 0.230, 0.232, 0.232 | 0.033, 0.027, 0.027 | 0 to 0 % | 0, 0, 0 |
| bulk | 9000 | 1 MiB | ZeroCopy | 3 | on | 16384 | 13.85, 13.80, 13.85 | 0.200, 0.203, 0.201 | 0.266, 0.265, 0.262 | 0.024, 0.036, 0.043 | 0 to 0 % | 0, 0, 0 |
| bulk | 8244 | 324 B | Ring | 1 | on | 16384 | 0.12, 0.13, 0.13 | 7.177, 6.876, 6.732 | 11.476, 10.770, 10.410 | 8.653, 7.927, 7.731 | - | 165, 187, 200 |
| bulk | 8244 | 324 B | Ring | 3 | on | 16384 | 0.28, 0.36, 0.34 | 6.890, 7.016, 7.284 | 14.344, 10.955, 12.109 | 4.879, 8.146, 5.837 | - | 44, 204, 159 |
| bulk | 8244 | 324 B | Direct | 1 | on | 16384 | 0.10, 0.11, 0.10 | 9.303, 8.968, 9.279 | 13.157, 12.633, 13.229 | 9.627, 9.209, 9.639 | - | 30, 93, 0 |
| bulk | 8244 | 324 B | Direct | 3 | on | 16384 | 0.30, 0.28, 0.32 | 9.285, 9.189, 8.842 | 13.852, 14.769, 13.001 | 8.524, 6.486, 7.636 | - | 89, 0, 300 |
| bulk | 8244 | 324 B | ZeroCopy | 1 | on | 16384 | 0.10, 0.09, 0.10 | 9.636, 10.605, 10.013 | 12.460, 14.323, 13.083 | 9.527, 11.167, 10.217 | 0 to 0 % | 94, 124, 0 |
| bulk | 8244 | 324 B | ZeroCopy | 3 | on | 16384 | 0.31, 0.25, 0.30 | 9.410, 9.493, 9.916 | 11.871, 14.245, 12.241 | 8.969, 5.489, 9.975 | 0 to 0 % | 97, 0, 159 |
| bulk | 8244 | 64 KiB | Ring | 1 | on | 16384 | 3.48, 4.39, 3.75 | 0.212, 0.211, 0.212 | 0.395, 0.314, 0.367 | 0.050, 0.061, 0.054 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | Ring | 3 | on | 16384 | 12.42, 12.55, 12.86 | 0.226, 0.225, 0.224 | 0.335, 0.331, 0.325 | 0.041, 0.047, 0.033 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | Direct | 1 | on | 16384 | 4.19, 4.76, 4.15 | 0.174, 0.174, 0.174 | 0.330, 0.291, 0.333 | 0.064, 0.084, 0.061 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | Direct | 3 | on | 16384 | 13.04, 12.68, 13.09 | 0.188, 0.189, 0.189 | 0.317, 0.325, 0.316 | 0.050, 0.047, 0.047 | - | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 16384 | 5.76, 5.71, 5.55 | 0.125, 0.125, 0.124 | 0.221, 0.225, 0.232 | 0.087, 0.088, 0.085 | 100 to 100 % | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 3 | on | 16384 | 13.02, 13.40, 13.11 | 0.131, 0.131, 0.132 | 0.280, 0.264, 0.275 | 0.060, 0.052, 0.055 | 100 to 100 % | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Ring | 1 | on | 16384 | 3.61, 2.54, 3.09 | 0.177, 0.187, 0.182 | 0.383, 0.539, 0.443 | 0.061, 0.059, 0.050 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Ring | 3 | on | 16384 | 9.73, 10.48, 9.73 | 0.199, 0.198, 0.199 | 0.426, 0.396, 0.427 | 0.043, 0.047, 0.050 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Direct | 1 | on | 16384 | 3.01, 3.10, 2.19 | 0.150, 0.149, 0.156 | 0.460, 0.447, 0.632 | 0.054, 0.045, 0.052 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | Direct | 3 | on | 16384 | 9.21, 11.29, 10.55 | 0.168, 0.165, 0.168 | 0.449, 0.368, 0.391 | 0.046, 0.042, 0.044 | - | 0, 0, 0 |
| bulk | 8244 | 1 MiB | ZeroCopy | 1 | on | 16384 | 6.57, 3.04, 2.20 | 0.076, 0.088, 0.093 | 0.198, 0.416, 0.595 | 0.051, 0.060, 0.045 | 100 to 100 % | 0, 0, 0 |
| bulk | 8244 | 1 MiB | ZeroCopy | 3 | on | 16384 | 10.52, 10.16, 12.05 | 0.090, 0.087, 0.086 | 0.340, 0.350, 0.296 | 0.050, 0.051, 0.048 | 100 to 100 % | 0, 0, 0 |

## What it shows

- **Small frames are a cost a frame, not a byte.** At 324 bytes every
  mode carries about 0.1 GB/s a core, some 330,000 frames a second, with
  the core busy; three cores carry three times that. No receive mode
  changes it, and zero-copy keeps nothing in pages (a frame shorter than
  `zc_min_frame` is moved by design).
- **Large frames that line up are where zero-copy pays.** At MTU 8244
  with frames of whole pages, every frame was kept in its pages. One
  core: 64 KiB frames at 5.6 to 5.8 GB/s and 0.125 CPU-s a GB, against
  direct mode's 4.2 to 4.8 at 0.174 and ring mode's 3.5 to 4.4 at 0.212;
  1 MiB frames at 0.076 to 0.093 against 0.15 and 0.18. With the queues'
  cores counted, 64 KiB frames cost 0.21 CPU-s a GB in zero-copy against
  0.24 direct and 0.26 ring.
- **Frames that do not line up lose by it.** At MTU 9000 nothing was
  kept, and zero-copy with 1 MiB frames carried 5.6 GB/s against direct
  mode's 7.4 on one core: a moved frame costs the move and the mode's
  pieces. Zero-copy is for traffic whose packets carry whole pages.
- **Three cores fill this interface in every mode.** 12.4 to 13.9 GB/s
  with frames of 64 KiB and 1 MiB, whatever the mode: the link, not the
  receive, is the limit with three cores here.
- **1 MiB frames at MTU 8244 are slower and vary** in every mode (2.2 to
  6.6 GB/s on one core; 9.2 to 12.1 on three), unlike MTU 9000. The
  cause was not found; the senders' own lines were not kept for these
  cells.

## The area, and the sender's option

| Kind | MTU | Frame | Mode | Cores | Sender's option | Spare | GB/s | Thread CPU-s a GB | Threads' cores | Queues' cores | Kept | Drops |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | off | 16384 | 3.68, 3.15, 3.38 | 0.118, 0.120, 0.120 | 0.345, 0.405, 0.380 | 0.079, 0.077, 0.075 | 99 to 100 % | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 4096 | 5.85, 5.05, 5.52 | 0.125, 0.127, 0.124 | 0.219, 0.255, 0.238 | 0.092, 0.088, 0.112 | 100 to 100 % | 0, 0, 0 |
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 1024 | 1.28, 0.00, 0.00 | 0.133, 0.000, 0.000 | 1.001, 0.000, 0.000 | 0.117, 0.000, 0.000 | 0 to 100 % | 5559, 3522, 2137 | errors: ['tcp_uring: the load did not finish: 0 of 2 connections came and closed']
| bulk | 8244 | 64 KiB | ZeroCopy | 1 | on | 0 | 0.00, 0.00, 0.00 | 0.000, 0.000, 0.000 | 0.000, 0.000, 0.000 | 0.000, 0.000, 0.000 | 0 to 0 % | 0, 0, 0 | errors: ['tcp_uring: the load did not finish: 0 of 2 connections came and closed']
| bulk | 8244 | 64 KiB | ZeroCopy | 3 | on | 0 | 0.00, 0.00, 0.00 | 0.000, 0.000, 0.000 | 0.000, 0.000, 0.000 | 0.000, 0.000, 0.000 | 0 to 0 % | 0, 0, 0 | errors: ['tcp_uring: the load did not finish: 0 of 2 connections came and closed']

- **An area smaller than the card's receive ring stops the queue.** The
  ring has 1,024 entries, and at MTU 8244 a packet fills two pages: about
  2,048 pages. With no spare (about 200 pages in all) no connection was
  accepted in 40 seconds, and `rx15_buff_alloc_err` counted the driver's
  failures to fill the ring; with 1,024 spare pages one run carried
  1.28 GB/s while the card dropped 5,559 packets, and in the two after it
  no connection came. With 4,096 spare pages the cell ran as with
  16,384, with no drop. `docs/tcp.md`'s rule of thumb is corrected to
  the ring's entries times the pages a packet fills.
- **The sender's option.** With `--frame-on-packet off`, 99 to 100 % of
  64 KiB frames were still kept, at 3.2 to 3.7 GB/s against 5.6 to 5.8
  with it; milestone 31 had 67 to 85 % with one connection. How many
  frames line up without the option depends on the flows; with it, all
  did in every run here.

## Relayed after `hold`: request and answer

`tcp_uring rr`: 324-byte requests on two connections, each answered
from frame memory after `hold`; MTU 8244, one core, three runs.

| Mode | Round trips a second | p50 µs | p99 µs | p99.9 µs |
|---|---|---|---|---|
| Ring | 52,638, 53,587, 54,847 | 32.8, 31.7, 32.8 | 164, 172, 148 | 623, 590, 492 |
| Direct | 52,234, 55,248, 52,211 | 32.8, 30.7, 34.8 | 164, 148, 156 | 524, 623, 557 |
| ZeroCopy | 44,056, 52,727, 42,278 | 34.8, 34.8, 34.8 | 410, 57, 442 | 852, 410, 819 |

Zero-copy gives small requests nothing and costs some: fewer round trips,
and a long tail in two runs of three.

## Not measured here

- Both interfaces at once, by the decision to leave the cluster's
  interface alone: the port's full 200 Gb/s is milestone 30's, with the
  kernel's copy.
- Writing to a log through `tcp_uring` in ring and direct mode; the log
  from pages is `zc_uring log`'s, milestone 31 (5.4 GB/s with 4 MiB
  writes, 0.09 to 0.11 CPU-s a GB).
- Frames checked over every byte: `bulk` reads one byte of a frame.
