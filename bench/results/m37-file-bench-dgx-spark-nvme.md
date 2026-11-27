# Milestone 37: longshore_file on a DGX Spark's NVMe disk

Task 4.1 of `add-file-crate`: `file_bench` writing and reading a file
through `longshore::file` ("file") and through `longshore_common`'s raw
`Op::File*` operations ("raw"), on the same backend and the same file, on
`donbot`: AArch64, Linux 7.0.0-1019-nvidia, a Samsung MZALC4T0HBL1 (3.7 TB
NVMe), ext4. Measured 2026-11-27. Raw lines:
`m37-file-bench-dgx-spark-nvme.jsonl`, one a pass.

## How
- `file_bench --direct --api both --phase both --write-size S --depth D
  --sync-every N --secs 3 --runs 3`, on a 1 GiB file: direct I/O
  (`O_DIRECT`), sequential offsets, block 4 KiB; both paths in every run,
  their order swapped run to run; three runs a cell. A write pass ends
  with one data sync (`--sync-every 0`) or syncs every 16 writes, at most
  one in flight; a read pass follows its write pass.
- `--backend uring` (`UringIo`) and `--backend posix` (`PosixIo` on
  `epoll`, which runs file operations one at a time, so depths above 4
  were not run on it).
- GB/s and CPU-seconds a GB of the bench's thread over the pass; p99 is
  the latency of a write or read, from submission to completion, of the
  first run (the histogram is within about 3 %).
- Built from `add-file-crate` at 165bf19, before the fixes of the review
  of groups 1 and 2, which do not touch the transfer path; the scripts:
  `bench/spikes/zcrx/group5/`'s style, here a loop over the options above.
- Ranges are the lowest and highest of the three runs.

## The ring, writes, one sync at the end

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.29 to 0.31 | 0.26 to 0.30 | 0.104 to 0.170 | 0.122 to 0.262 | 819 | 836 |
| 64 KiB | 4 | 1.07 to 1.16 | 0.76 to 1.22 | 0.057 to 0.081 | 0.041 to 0.149 | 803 | 541 |
| 64 KiB | 16 | 4.23 to 4.95 | 3.70 to 4.34 | 0.033 to 0.045 | 0.030 to 0.051 | 590 | 1049 |
| 64 KiB | 32 | 9.21 to 9.74 | 7.86 to 8.92 | 0.023 to 0.029 | 0.025 to 0.036 | 573 | 999 |
| 1024 KiB | 1 | 3.96 to 4.78 | 3.96 to 4.50 | 0.011 to 0.020 | 0.019 to 0.024 | 1016 | 786 |
| 1024 KiB | 4 | 1.82 to 1.94 | 1.82 to 1.87 | 0.031 to 0.042 | 0.042 to 0.046 | 4194 | 3998 |
| 1024 KiB | 16 | 1.83 to 2.59 | 1.89 to 2.63 | 0.035 to 0.043 | 0.027 to 0.039 | 11534 | 12321 |
| 1024 KiB | 32 | 1.78 to 1.87 | 1.80 to 1.88 | 0.036 to 0.046 | 0.028 to 0.046 | 23593 | 22544 |

## The ring, writes, a sync every 16 writes

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.21 to 0.22 | 0.21 to 0.23 | 0.331 to 0.372 | 0.288 to 0.369 | 1245 | 1212 |
| 64 KiB | 4 | 0.60 to 0.65 | 0.54 to 0.67 | 0.140 to 0.189 | 0.131 to 0.205 | 1573 | 1475 |
| 64 KiB | 16 | 1.80 to 1.86 | 1.61 to 1.81 | 0.066 to 0.070 | 0.074 to 0.086 | 1802 | 1966 |
| 64 KiB | 32 | 2.89 to 3.09 | 2.90 to 3.08 | 0.054 to 0.065 | 0.042 to 0.061 | 2032 | 1999 |
| 1024 KiB | 1 | 1.26 to 2.30 | 1.26 to 1.33 | 0.028 to 0.053 | 0.046 to 0.051 | 2621 | 3146 |
| 1024 KiB | 4 | 1.45 to 1.49 | 1.44 to 1.50 | 0.041 to 0.057 | 0.036 to 0.050 | 6292 | 6292 |
| 1024 KiB | 16 | 1.46 to 2.15 | 1.47 to 1.49 | 0.030 to 0.044 | 0.036 to 0.042 | 16515 | 17826 |
| 1024 KiB | 32 | 1.72 to 2.37 | 1.62 to 2.33 | 0.027 to 0.032 | 0.031 to 0.046 | 32506 | 34603 |

## The ring, reads

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.26 to 0.29 | 0.27 to 0.29 | 0.159 to 0.244 | 0.133 to 0.195 | 770 | 852 |
| 64 KiB | 4 | 0.99 to 1.24 | 1.05 to 1.15 | 0.048 to 0.091 | 0.058 to 0.075 | 573 | 786 |
| 64 KiB | 16 | 3.81 to 4.79 | 4.05 to 5.16 | 0.024 to 0.048 | 0.020 to 0.044 | 688 | 901 |
| 64 KiB | 32 | 8.16 to 9.48 | 7.85 to 9.99 | 0.025 to 0.035 | 0.018 to 0.026 | 623 | 770 |
| 1024 KiB | 1 | 4.29 to 4.62 | 4.33 to 4.77 | 0.014 to 0.019 | 0.011 to 0.022 | 786 | 737 |
| 1024 KiB | 4 | 10.77 to 11.04 | 9.76 to 10.75 | 0.012 to 0.013 | 0.017 to 0.017 | 786 | 1147 |
| 1024 KiB | 16 | 13.38 to 13.43 | 13.40 to 13.66 | 0.015 to 0.015 | 0.008 to 0.015 | 1802 | 1671 |
| 1024 KiB | 32 | 13.36 to 13.41 | 13.37 to 13.49 | 0.014 to 0.018 | 0.009 to 0.014 | 3146 | 2884 |

## The portable backend (`epoll`), writes and reads, one sync at the end

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.26 to 0.30 | 0.29 to 0.31 | 0.164 to 0.280 | 0.109 to 0.172 | 688 | 623 |
| 64 KiB | 4 | 0.25 to 0.28 | 0.26 to 0.30 | 0.169 to 0.311 | 0.133 to 0.279 | 2556 | 2163 |
| 1024 KiB | 1 | 1.51 to 1.61 | 1.50 to 1.54 | 0.061 to 0.072 | 0.065 to 0.067 | 1770 | 1933 |
| 1024 KiB | 4 | 1.63 to 1.81 | 1.66 to 1.81 | 0.056 to 0.072 | 0.061 to 0.066 | 4588 | 3736 |

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.24 to 0.28 | 0.24 to 0.30 | 0.223 to 0.338 | 0.118 to 0.270 | 934 | 754 |
| 64 KiB | 4 | 0.17 to 0.22 | 0.18 to 0.21 | 0.277 to 0.454 | 0.276 to 0.427 | 2818 | 3408 |
| 1024 KiB | 1 | 2.99 to 4.65 | 3.50 to 4.36 | 0.030 to 0.056 | 0.034 to 0.043 | 786 | 819 |
| 1024 KiB | 4 | 2.49 to 2.66 | 2.29 to 2.47 | 0.050 to 0.057 | 0.058 to 0.061 | 3998 | 3604 |

## Durable writes (`OpenFlags::with_dsync`), 64 KiB, depth 16

| Backend | GB/s, file | GB/s, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|
| uring | 0.28 to 0.28 | 0.27 to 0.28 | 5243 | 5505 |
| posix | 0.02 | 0.02 | 55574 | 53477 |

## What it shows

- **The crate costs nothing measurable over the raw operations.** In
  every cell of both backends the two paths' GB/s ranges overlap or the
  crate's is higher, and their CPU a GB lie in the same range (on the
  ring 0.01 to 0.05 CPU-seconds a GB from depth 4 up). Three runs a cell
  do not resolve differences of a few per cent either way.
- **Depth is what the disk needs.** On the ring, reads of 1 MiB reach
  13.4 GB/s from depth 16, and writes of 64 KiB 9.2 to 9.7 GB/s at depth
  32 (7.9 to 8.9 raw); at depth 1 a 64 KiB transfer is about 0.3 GB/s on
  either path.
- **Writes of 1 MiB stay at 1.8 to 2.6 GB/s** at every depth from 4, on
  both paths, below what 64 KiB writes reach. That is the disk's (or the
  file system's) behaviour with large direct writes on this machine, not
  the crate's; it was not looked into further.
- **The portable backend** reaches 1.5 to 1.8 GB/s writing and 2.3 to
  4.6 GB/s reading with 1 MiB transfers, and about 0.3 with 64 KiB:
  operations one at a time.
- **Syncs every 16 writes** cost a third to two thirds of the write rate
  at 64 KiB; durable writes, each its own sync, come to 0.28 GB/s on the
  ring and 0.02 on the portable backend.

## Not measured here
Buffered I/O, random offsets, more than one file, and the crate's
sequences and gathered writes, which `file_bench` does not drive; this
Mac's figures are milestone 36.
