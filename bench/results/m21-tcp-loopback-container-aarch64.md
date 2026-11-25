# TCP connection set: loopback in a Linux container

`tcp_uring rr`, `bulk` and `path` (see `crates/longshore-bench/src/bin/tcp_uring/bench.rs`),
run by `scripts/bench-tcp.sh --secs 3 --runs 3`. Raw runs:
`m21-tcp-loopback-container-aarch64.jsonl`.

**Host:** a Linux container on a laptop (Apple silicon, OrbStack, kernel
7.2.12, AArch64), both ends on loopback, the thread under test and its peer
not pinned. **What this can tell:** which setting is better than which on this
machine. **What it cannot:** what a network card carries, what zero-copy is
worth (loopback copies whatever it is asked), or what a real disk does to the
log. Those runs are to be made on hosts with 200 and 300 Gb/s cards, with the
same tool; the defaults taken from this file are provisional until then.

## What it shows

- **The ring against epoll, same code.** Request and response at 324 bytes:
  about 1.0 million a second on the ring at 64 to 256 connections against
  0.38 to 0.50 million on epoll. At 4,096 connections 0.60 against 0.26
  million, and the 999th thousandth of answers within 11 ms against 419 ms.
  Bulk at 64 KiB frames: 11.5 against 7.6 GB/s (14.7 with 16 KiB ring
  buffers).
- **One connection is the exception.** With a single connection and a thread
  that waits, epoll answers faster here (p50 4 µs against 24 µs). Spinning
  brings the ring to 8 µs at the cost of a core. Not explained yet.
- **Receive mode.** Ring mode is ahead for frames smaller than a ring buffer
  (0.89 against 0.62 million a second at 324 bytes); direct mode is level or
  slightly ahead from 4 KiB frames, and well ahead for 1 MiB frames in bulk
  (9.9 against 6.2 GB/s).
- **Ring buffer size.** 16 KiB carries most in bulk (14.7 GB/s, against 11.5
  at 4 KiB and 10.0 at 64 KiB) and costs small requests little (0.84 against
  0.89 million a second). **Default set to 16 KiB, from 64 KiB.**
- **Send segment size.** 64 KiB and 256 KiB are level; 1 MiB segments are
  much slower with 64 KiB frames (6.2 GB/s), because they leave as 1 MiB
  zero-copy sends. Default left at 64 KiB.
- **Zero-copy threshold.** Not decidable on loopback. For 64 KiB frames
  zero-copy is slightly ahead (14.8 against 13.3 GB/s); for 1 MiB frames it
  is far behind (6 against 11.6 GB/s). Default left at 16 KiB, to be set on a
  network card.
- **Connections.** The rate holds to 1,024 connections and falls by about a
  third at 4,096, with one request in flight on each.
- **The whole path** (a block received, written with direct I/O, made
  durable, sent to three replicas, answered). On this machine's virtual disk
  a block takes one to five milliseconds and the runs scatter by a factor of
  two to four, so neither a sync after each write against a file opened for
  durable writes, nor waiting against spinning, can be told apart. No default
  is taken from them.

## Every configuration

Median of the runs, lowest and highest in brackets. GB is 10^9 bytes; times
are of a request and its answer.

| cmd | backend | rx | wait | sync | conns | threads | frame | ring | zc_min | segment | arch | runs | GB/s (median, low to high) | per s (median, low to high) | p50 µs (median, low to high) | p99 µs (median, low to high) | p999 µs (median, low to high) | CPU-s per GB (median, low to high) | peer CPU-s per GB (median, low to high) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| rr | uring | Ring | sleep | - | 1 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 6 | 0.01 (0.01 to 0.02) | 43342 (40213 to 56371) | 23.6 (22.5 to 23.6) | 34.8 (32.8 to 36.9) | 60.4 (51.2 to 81.9) | 24.207 (18.784 to 25.793) | 23.555 (18.331 to 25.178) |
| rr | uring | Ring | sleep | - | 16 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 9 | 0.29 (0.25 to 0.29) | 893452 (769729 to 904042) | 16.4 (16.4 to 17.4) | 32.8 (29.7 to 59.4) | 59.4 (51.2 to 180.2) | 3.432 (3.401 to 3.879) | 3.432 (3.401 to 3.805) |
| rr | uring | Ring | sleep | - | 64 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.33 (0.31 to 0.33) | 1018561 (958678 to 1021492) | 59.4 (59.4 to 61.4) | 94.2 (90.1 to 163.8) | 172.0 (163.8 to 294.9) | 3.029 (3.019 to 3.206) | 3.027 (3.018 to 3.203) |
| rr | uring | Ring | sleep | - | 256 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.35 (0.34 to 0.35) | 1067283 (1056314 to 1067571) | 221.2 (221.2 to 229.4) | 294.9 (278.5 to 311.3) | 524.3 (458.8 to 720.9) | 2.889 (2.888 to 2.919) | 2.886 (2.883 to 2.915) |
| rr | uring | Ring | sleep | - | 1024 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.31 (0.29 to 0.31) | 948660 (898729 to 949055) | 1015.8 (1015.8 to 1048.6) | 1245.2 (1179.6 to 1703.9) | 1900.5 (1245.2 to 2359.3) | 3.246 (3.244 to 3.432) | 3.235 (3.233 to 3.419) |
| rr | uring | Ring | sleep | - | 4096 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.19 (0.19 to 0.20) | 598315 (593463 to 602040) | 6029.3 (5767.2 to 6029.3) | 8912.9 (8912.9 to 9437.2) | 11010.0 (10485.8 to 11534.3) | 5.147 (5.104 to 5.166) | 4.504 (4.489 to 4.516) |
| rr | uring | Ring | sleep | - | 16 | 1 | 1284 | 4096 | 16384 | 65536 | aarch64 | 3 | 1.11 (1.10 to 1.12) | 861210 (854008 to 869933) | 17.4 (17.4 to 17.4) | 31.7 (27.6 to 32.8) | 51.2 (45.1 to 55.3) | 0.900 (0.888 to 0.909) | 0.900 (0.888 to 0.908) |
| rr | epoll | Ring | sleep | - | 1 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.07 (0.07 to 0.07) | 229700 (227278 to 231491) | 4.1 (4.1 to 4.1) | 10.2 (8.7 to 11.3) | 16.4 (14.3 to 17.4) | 6.749 (6.671 to 6.829) | 6.680 (6.656 to 6.749) |
| rr | epoll | Ring | sleep | - | 16 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.11 (0.11 to 0.11) | 338368 (337471 to 341668) | 45.1 (45.1 to 45.1) | 63.5 (61.4 to 63.5) | 81.9 (81.9 to 98.3) | 4.656 (4.632 to 4.692) | 4.665 (4.627 to 4.689) |
| rr | epoll | Ring | sleep | - | 64 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.16 (0.16 to 0.17) | 500540 (494312 to 509811) | 114.7 (114.7 to 114.7) | 204.8 (204.8 to 204.8) | 237.6 (237.6 to 245.8) | 5.489 (5.443 to 5.527) | 5.488 (5.443 to 5.524) |
| rr | epoll | Ring | sleep | - | 256 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.12 (0.12 to 0.13) | 377704 (365964 to 403718) | 753.7 (524.3 to 852.0) | 983.0 (950.3 to 1015.8) | 1507.3 (1376.3 to 1638.4) | 5.546 (5.517 to 5.591) | 5.420 (5.410 to 5.519) |
| rr | epoll | Ring | sleep | - | 1024 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.10 (0.09 to 0.10) | 297703 (280581 to 309418) | 3014.7 (2752.5 to 3145.7) | 6815.7 (6029.3 to 7864.3) | 8912.9 (8126.5 to 11010.0) | 7.692 (7.322 to 8.248) | 7.356 (7.004 to 7.840) |
| rr | epoll | Ring | sleep | - | 4096 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.08 (0.08 to 0.09) | 257779 (252782 to 267020) | 6029.3 (6029.3 to 6029.3) | 83886.1 (79691.8 to 109051.9) | 419430.4 (335544.3 to 436207.6) | 10.616 (10.205 to 10.776) | 9.260 (9.035 to 9.289) |
| rr | epoll | Ring | sleep | - | 16 | 1 | 1284 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.40 (0.39 to 0.41) | 311752 (305130 to 316792) | 49.2 (49.2 to 49.2) | 73.7 (69.6 to 77.8) | 155.6 (90.1 to 172.0) | 1.259 (1.254 to 1.293) | 1.278 (1.265 to 1.307) |
| rr | uring | Ring | sleep | - | 16 | 1 | 324 | 1024 | 16384 | 65536 | aarch64 | 3 | 0.29 (0.29 to 0.30) | 905011 (901783 to 914361) | 16.4 (16.4 to 16.4) | 29.7 (24.6 to 29.7) | 49.2 (43.0 to 61.4) | 3.397 (3.357 to 3.399) | 3.396 (3.357 to 3.399) |
| rr | uring | Ring | sleep | - | 16 | 1 | 4356 | 1024 | 16384 | 65536 | aarch64 | 3 | 2.21 (2.12 to 2.23) | 506307 (487057 to 511290) | 26.6 (26.6 to 29.7) | 77.8 (73.7 to 81.9) | 139.3 (102.4 to 155.6) | 0.431 (0.428 to 0.455) | 0.430 (0.427 to 0.441) |
| rr | uring | Ring | sleep | - | 16 | 1 | 4356 | 4096 | 16384 | 65536 | aarch64 | 3 | 2.49 (2.34 to 2.67) | 571125 (537788 to 613336) | 23.6 (23.6 to 24.6) | 69.6 (69.6 to 81.9) | 98.3 (90.1 to 163.8) | 0.387 (0.364 to 0.402) | 0.386 (0.364 to 0.399) |
| rr | uring | Ring | sleep | - | 16 | 1 | 324 | 16384 | 16384 | 65536 | aarch64 | 3 | 0.27 (0.27 to 0.29) | 838662 (834649 to 896372) | 16.4 (16.4 to 17.4) | 49.2 (31.7 to 49.2) | 98.3 (61.4 to 102.4) | 3.613 (3.415 to 3.640) | 3.607 (3.415 to 3.637) |
| rr | uring | Ring | sleep | - | 16 | 1 | 4356 | 16384 | 16384 | 65536 | aarch64 | 3 | 2.44 (2.07 to 2.52) | 561031 (474454 to 577863) | 23.6 (23.6 to 25.6) | 90.1 (81.9 to 106.5) | 155.6 (155.6 to 221.2) | 0.386 (0.372 to 0.448) | 0.391 (0.377 to 0.454) |
| rr | uring | Direct | sleep | - | 16 | 1 | 324 | 1024 | 16384 | 65536 | aarch64 | 3 | 0.20 (0.20 to 0.21) | 622157 (607213 to 649505) | 22.5 (22.5 to 23.6) | 65.5 (59.4 to 69.6) | 131.1 (122.9 to 139.3) | 4.791 (4.655 to 4.983) | 4.780 (4.644 to 4.967) |
| rr | uring | Direct | sleep | - | 16 | 1 | 4356 | 1024 | 16384 | 65536 | aarch64 | 3 | 2.41 (2.32 to 2.43) | 552274 (532944 to 557606) | 26.6 (25.6 to 26.6) | 65.5 (65.5 to 69.6) | 147.5 (122.9 to 163.8) | 0.411 (0.405 to 0.421) | 0.410 (0.404 to 0.420) |
| rr | uring | Direct | sleep | - | 16 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.20 (0.20 to 0.20) | 614454 (614108 to 625802) | 22.5 (21.5 to 22.5) | 63.5 (61.4 to 65.5) | 163.8 (131.1 to 221.2) | 4.794 (4.724 to 4.848) | 4.777 (4.712 to 4.836) |
| rr | uring | Direct | sleep | - | 16 | 1 | 4356 | 4096 | 16384 | 65536 | aarch64 | 3 | 2.58 (2.38 to 2.60) | 592981 (545253 to 597664) | 25.6 (24.6 to 25.6) | 53.2 (49.2 to 73.7) | 106.5 (90.1 to 147.5) | 0.386 (0.381 to 0.414) | 0.385 (0.381 to 0.413) |
| rr | uring | Direct | sleep | - | 16 | 1 | 324 | 16384 | 16384 | 65536 | aarch64 | 3 | 0.21 (0.20 to 0.22) | 644246 (615010 to 690173) | 22.5 (21.5 to 23.6) | 57.3 (51.2 to 65.5) | 98.3 (69.6 to 147.5) | 4.680 (4.412 to 4.859) | 4.669 (4.412 to 4.839) |
| rr | uring | Direct | sleep | - | 16 | 1 | 4356 | 16384 | 16384 | 65536 | aarch64 | 3 | 2.61 (2.59 to 2.62) | 599189 (595189 to 601762) | 25.6 (25.6 to 25.6) | 43.0 (38.9 to 53.2) | 77.8 (73.7 to 86.0) | 0.382 (0.380 to 0.384) | 0.382 (0.379 to 0.384) |
| rr | uring | Ring | spin | - | 1 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.04 (0.04 to 0.04) | 118606 (116564 to 121619) | 7.7 (7.7 to 7.9) | 18.4 (15.4 to 20.5) | 25.6 (25.6 to 34.8) | 26.018 (25.370 to 26.468) | 10.587 (10.203 to 10.798) |
| rr | uring | Ring | spin | - | 16 | 1 | 324 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.29 (0.29 to 0.29) | 889450 (882185 to 900143) | 16.4 (16.4 to 16.4) | 30.7 (28.7 to 38.9) | 53.2 (47.1 to 57.3) | 3.470 (3.429 to 3.498) | 3.462 (3.424 to 3.491) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 4096 | 16384 | 65536 | aarch64 | 6 | 11.51 (11.26 to 11.58) | 175642 (171789 to 176746) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.075 (0.074 to 0.076) | 0.086 (0.086 to 0.088) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 1048576 | 4096 | 16384 | 1048576 | aarch64 | 3 | 6.10 (5.50 to 9.10) | 5815 (5243 to 8677) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.061 (0.059 to 0.065) | 0.163 (0.108 to 0.179) |
| bulk | epoll | Ring | sleep | - | 4 | 1 | 65536 | 4096 | 16384 | 65536 | aarch64 | 3 | 7.55 (7.45 to 7.72) | 115220 (113660 to 117739) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.054 (0.053 to 0.056) | 0.132 (0.130 to 0.134) |
| bulk | epoll | Ring | sleep | - | 4 | 1 | 1048576 | 4096 | 16384 | 1048576 | aarch64 | 3 | 7.33 (6.98 to 7.59) | 6987 (6655 to 7238) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.050 (0.049 to 0.052) | 0.136 (0.132 to 0.143) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 16384 | 16384 | 65536 | aarch64 | 12 | 14.74 (13.66 to 14.95) | 224846 (208420 to 228148) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.065 (0.065 to 0.070) | 0.066 (0.065 to 0.070) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 1048576 | 16384 | 16384 | 1048576 | aarch64 | 6 | 6.22 (5.46 to 7.75) | 5928 (5208 to 7387) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.040 (0.031 to 0.053) | 0.161 (0.129 to 0.183) |
| bulk | uring | Direct | sleep | - | 4 | 1 | 65536 | 16384 | 16384 | 65536 | aarch64 | 3 | 12.89 (12.88 to 13.00) | 196622 (196470 to 198388) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.075 (0.075 to 0.075) | 0.058 (0.058 to 0.061) |
| bulk | uring | Direct | sleep | - | 4 | 1 | 1048576 | 16384 | 16384 | 1048576 | aarch64 | 3 | 9.85 (8.38 to 10.54) | 9398 (7988 to 10052) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.064 (0.057 to 0.065) | 0.094 (0.091 to 0.118) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 65536 | 16384 | 65536 | aarch64 | 3 | 10.01 (9.75 to 10.02) | 152734 (148792 to 152905) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.081 (0.080 to 0.081) | 0.072 (0.071 to 0.072) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 16384 | 4096 | 65536 | aarch64 | 3 | 14.84 (14.80 to 15.01) | 226372 (225850 to 228983) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.065 (0.065 to 0.065) | 0.066 (0.065 to 0.066) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 1048576 | 16384 | 4096 | 1048576 | aarch64 | 3 | 5.92 (5.41 to 6.57) | 5646 (5163 to 6265) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.030 (0.027 to 0.045) | 0.169 (0.152 to 0.185) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 16384 | 65536 | 65536 | aarch64 | 3 | 14.86 (14.34 to 14.88) | 226787 (218773 to 227083) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.065 (0.065 to 0.067) | 0.066 (0.065 to 0.068) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 1048576 | 16384 | 65536 | 1048576 | aarch64 | 3 | 5.48 (5.21 to 6.70) | 5225 (4970 to 6387) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.032 (0.032 to 0.046) | 0.182 (0.149 to 0.192) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 16384 | 4294967295 | 65536 | aarch64 | 3 | 13.33 (13.16 to 13.37) | 203360 (200854 to 203973) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.040 (0.040 to 0.041) | 0.075 (0.075 to 0.076) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 1048576 | 16384 | 4294967295 | 1048576 | aarch64 | 3 | 11.55 (11.29 to 11.71) | 11020 (10764 to 11166) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.055 (0.050 to 0.055) | 0.086 (0.085 to 0.088) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 16384 | 16384 | 262144 | aarch64 | 3 | 14.63 (14.54 to 15.33) | 223313 (221920 to 233903) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.061 (0.058 to 0.062) | 0.067 (0.065 to 0.068) |
| bulk | uring | Ring | sleep | - | 4 | 1 | 65536 | 16384 | 16384 | 1048576 | aarch64 | 3 | 6.21 (6.08 to 6.53) | 94676 (92826 to 99632) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.0 (0.0 to 0.0) | 0.033 (0.032 to 0.036) | 0.161 (0.152 to 0.164) |
| path | uring | Ring | sleep | fsync | 1 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.00 (0.00 to 0.00) | 400 (351 to 447) | 2359.3 (2031.6 to 2490.4) | 4194.3 (4194.3 to 4980.7) | 67108.9 (62914.6 to 71303.2) | 14.286 (12.111 to 17.580) | 50.383 (38.821 to 69.491) |
| path | uring | Ring | sleep | fsync | 16 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.01 (0.01 to 0.02) | 2894 (2778 to 4626) | 4980.7 (3932.2 to 5242.9) | 9437.2 (7340.0 to 13107.2) | 92274.7 (13107.2 to 96469.0) | 4.066 (2.718 to 4.718) | 21.756 (16.063 to 23.435) |
| path | uring | Ring | sleep | dsync | 1 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 6 | 0.00 (0.00 to 0.00) | 527 (200 to 908) | 2261.0 (983.0 to 4456.4) | 5898.2 (1572.9 to 15728.6) | 9699.3 (5242.9 to 79691.8) | 8.690 (2.139 to 24.022) | 98.382 (38.617 to 280.655) |
| path | uring | Ring | sleep | dsync | 16 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.02 (0.01 to 0.02) | 4539 (1905 to 5242) | 2359.3 (2359.3 to 7340.0) | 17825.8 (14155.8 to 44040.2) | 83886.1 (30408.7 to 83886.1) | 1.929 (1.545 to 4.854) | 11.254 (9.950 to 26.873) |
| path | epoll | Ring | sleep | fsync | 1 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.00 (0.00 to 0.01) | 863 (482 to 1610) | 589.8 (557.1 to 1900.5) | 3276.8 (950.3 to 5242.9) | 9437.2 (7864.3 to 12582.9) | 6.572 (3.874 to 12.220) | 48.728 (25.132 to 98.011) |
| path | epoll | Ring | sleep | fsync | 16 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.01 (0.00 to 0.01) | 1693 (448 to 1730) | 11010.0 (9961.5 to 35651.6) | 28311.6 (19922.9 to 167772.2) | 67108.9 (23068.7 to 201326.6) | 3.424 (3.178 to 14.447) | 24.800 (22.652 to 123.257) |
| path | epoll | Ring | sleep | dsync | 1 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.00 (0.00 to 0.00) | 514 (217 to 828) | 1114.1 (1048.6 to 4063.2) | 6029.3 (6029.3 to 7340.0) | 54526.0 (9437.2 to 79691.8) | 6.854 (3.877 to 22.114) | 91.140 (41.164 to 237.576) |
| path | epoll | Ring | sleep | dsync | 16 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.00 (0.00 to 0.00) | 862 (859 to 875) | 22020.1 (22020.1 to 23068.7) | 37748.7 (35651.6 to 37748.7) | 41943.0 (41943.0 to 48234.5) | 3.534 (3.421 to 3.649) | 43.003 (41.446 to 43.984) |
| path | uring | Ring | spin | dsync | 1 | 1 | 4096 | 4096 | 16384 | 65536 | aarch64 | 3 | 0.00 (0.00 to 0.00) | 839 (269 to 901) | 1048.6 (983.0 to 3407.9) | 3801.1 (3407.9 to 9437.2) | 9437.2 (8912.9 to 88080.4) | 290.397 (269.801 to 905.374) | 43.128 (42.284 to 48.011) |
