# Milestone 37: longshore_file on a DGX Spark's NVMe disk

Task 4.1 of `add-file-crate`: `file_bench` writing and reading a file
through `longshore::file` ("file") and through `longshore_common`'s raw
`Op::File*` operations ("raw"), on the same backend and the same file, on
`donbot`: AArch64, Linux 7.0.0-1019-nvidia, a Samsung MZALC4T0HBL1 (3.7 TB
NVMe), ext4. Measured 2026-11-27. Raw lines:
`m37-file-bench-dgx-spark-nvme.jsonl`, one a pass; corrected after the
review of group 4.

## How
- `file_bench --direct --api both --write-size S --depth D --sync-every N
  --secs 3 --runs 3` on one 1 GiB file, with `--phase both` (a write pass,
  then a read pass), except the durable-write cells, `--phase write`.
  Direct I/O (`O_DIRECT`), sequential offsets, block 4 KiB; three runs a
  cell. The matrix is `bench/spikes/file/fbm.sh`, run on `donbot` from its
  copy of the tree.
- Both paths run in every run on the same backend, the set's file in file
  slot 1 and the raw path's in slot 0; the order alternates, so with three
  runs the set goes first in runs 1 and 3, raw in run 2.
- A pass is three seconds of transfers on a 1 GiB region, so the disk sees
  the same region rewritten many times (about 28 times a pass at
  9.7 GB/s): these are figures for a hot region, not for a disk filling up.
- GB/s and CPU-seconds a GB of the bench's thread over the pass; on
  `PosixIo` the file operations run synchronously on that thread, so its
  CPU includes the kernel's work. p99 is a write's or read's latency from
  submission to completion; the tables give the median of the three runs'
  p99s.
- Built from `add-file-crate` at 165bf19, before the fixes of the review of
  groups 1 and 2 (5c0bf52): those change short transfers and error paths,
  which no pass here met (every transfer completed whole).
- `--backend posix` runs file operations one at a time, so depths above 4
  were not run on it. Ranges are the lowest and highest of three runs.

### The ring (`UringIo`), writes, one data sync at the end of the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.29 to 0.31 | 0.26 to 0.30 | 0.104 to 0.170 | 0.122 to 0.262 | 655 | 655 |
| 64 KiB | 4 | 1.07 to 1.16 | 0.76 to 1.22 | 0.057 to 0.081 | 0.041 to 0.149 | 803 | 983 |
| 64 KiB | 16 | 4.23 to 4.95 | 3.70 to 4.34 | 0.033 to 0.045 | 0.030 to 0.051 | 590 | 1016 |
| 64 KiB | 32 | 9.21 to 9.74 | 7.86 to 8.92 | 0.023 to 0.029 | 0.025 to 0.036 | 573 | 885 |
| 1024 KiB | 1 | 3.96 to 4.78 | 3.96 to 4.50 | 0.011 to 0.020 | 0.019 to 0.024 | 754 | 803 |
| 1024 KiB | 4 | 1.82 to 1.94 | 1.82 to 1.87 | 0.031 to 0.042 | 0.042 to 0.046 | 3932 | 4063 |
| 1024 KiB | 16 | 1.83 to 2.59 | 1.89 to 2.63 | 0.035 to 0.043 | 0.027 to 0.039 | 12059 | 11534 |
| 1024 KiB | 32 | 1.78 to 1.87 | 1.80 to 1.88 | 0.036 to 0.046 | 0.028 to 0.046 | 23593 | 23593 |

### The ring (`UringIo`), writes, a data sync every 16 writes in the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.21 to 0.22 | 0.21 to 0.23 | 0.331 to 0.372 | 0.288 to 0.369 | 1245 | 1278 |
| 64 KiB | 4 | 0.60 to 0.65 | 0.54 to 0.67 | 0.140 to 0.189 | 0.131 to 0.205 | 1573 | 1507 |
| 64 KiB | 16 | 1.80 to 1.86 | 1.61 to 1.81 | 0.066 to 0.070 | 0.074 to 0.086 | 1802 | 1966 |
| 64 KiB | 32 | 2.89 to 3.09 | 2.90 to 3.08 | 0.054 to 0.065 | 0.042 to 0.061 | 1999 | 1999 |
| 1024 KiB | 1 | 1.26 to 2.30 | 1.26 to 1.33 | 0.028 to 0.053 | 0.046 to 0.051 | 3146 | 3146 |
| 1024 KiB | 4 | 1.45 to 1.49 | 1.44 to 1.50 | 0.041 to 0.057 | 0.036 to 0.050 | 6292 | 6292 |
| 1024 KiB | 16 | 1.46 to 2.15 | 1.47 to 1.49 | 0.030 to 0.044 | 0.036 to 0.042 | 16777 | 17302 |
| 1024 KiB | 32 | 1.72 to 2.37 | 1.62 to 2.33 | 0.027 to 0.032 | 0.031 to 0.046 | 32506 | 34603 |

### The ring (`UringIo`), reads after the write pass with one data sync at the end of the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.26 to 0.29 | 0.27 to 0.29 | 0.159 to 0.244 | 0.133 to 0.195 | 885 | 803 |
| 64 KiB | 4 | 0.99 to 1.24 | 1.05 to 1.15 | 0.048 to 0.091 | 0.058 to 0.075 | 672 | 786 |
| 64 KiB | 16 | 3.81 to 4.79 | 4.05 to 5.16 | 0.024 to 0.048 | 0.020 to 0.044 | 786 | 311 |
| 64 KiB | 32 | 8.16 to 9.48 | 7.85 to 9.99 | 0.025 to 0.035 | 0.018 to 0.026 | 803 | 770 |
| 1024 KiB | 1 | 4.29 to 4.62 | 4.33 to 4.77 | 0.014 to 0.019 | 0.011 to 0.022 | 786 | 737 |
| 1024 KiB | 4 | 10.77 to 11.04 | 9.76 to 10.75 | 0.012 to 0.013 | 0.017 to 0.017 | 918 | 1081 |
| 1024 KiB | 16 | 13.38 to 13.43 | 13.40 to 13.66 | 0.015 to 0.015 | 0.008 to 0.015 | 1770 | 1737 |
| 1024 KiB | 32 | 13.36 to 13.41 | 13.37 to 13.49 | 0.014 to 0.018 | 0.009 to 0.014 | 3146 | 2949 |

### The ring (`UringIo`), reads after the write pass with a data sync every 16 writes in the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.28 to 0.29 | 0.25 to 0.31 | 0.151 to 0.171 | 0.094 to 0.253 | 721 | 704 |
| 64 KiB | 4 | 0.90 to 1.26 | 1.07 to 1.21 | 0.030 to 0.116 | 0.044 to 0.077 | 721 | 704 |
| 64 KiB | 16 | 3.70 to 5.05 | 3.70 to 4.44 | 0.019 to 0.046 | 0.025 to 0.050 | 836 | 950 |
| 64 KiB | 32 | 8.96 to 9.16 | 8.32 to 10.01 | 0.022 to 0.030 | 0.017 to 0.027 | 786 | 623 |
| 1024 KiB | 1 | 3.30 to 4.89 | 4.05 to 4.48 | 0.011 to 0.032 | 0.015 to 0.021 | 901 | 770 |
| 1024 KiB | 4 | 10.39 to 10.48 | 9.75 to 10.70 | 0.015 to 0.017 | 0.015 to 0.018 | 1016 | 999 |
| 1024 KiB | 16 | 12.78 to 12.98 | 12.85 to 12.90 | 0.013 to 0.017 | 0.013 to 0.015 | 1900 | 1966 |
| 1024 KiB | 32 | 12.94 to 14.76 | 12.84 to 12.99 | 0.011 to 0.017 | 0.010 to 0.016 | 3211 | 3211 |

### The portable backend (`PosixIo` on `epoll`), writes, one data sync at the end of the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.26 to 0.30 | 0.29 to 0.31 | 0.164 to 0.280 | 0.109 to 0.172 | 688 | 623 |
| 64 KiB | 4 | 0.25 to 0.28 | 0.26 to 0.30 | 0.169 to 0.311 | 0.133 to 0.279 | 2556 | 2687 |
| 1024 KiB | 1 | 1.51 to 1.61 | 1.50 to 1.54 | 0.061 to 0.072 | 0.065 to 0.067 | 1770 | 1835 |
| 1024 KiB | 4 | 1.63 to 1.81 | 1.66 to 1.81 | 0.056 to 0.072 | 0.061 to 0.066 | 3998 | 3736 |

### The portable backend (`PosixIo` on `epoll`), writes, a data sync every 16 writes in the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.14 to 0.15 | 0.15 to 0.16 | 0.410 to 0.486 | 0.315 to 0.375 | 3408 | 3277 |
| 64 KiB | 4 | 0.15 to 0.15 | 0.15 to 0.15 | 0.371 to 0.504 | 0.368 to 0.448 | 5243 | 5243 |
| 1024 KiB | 1 | 1.08 to 1.26 | 1.17 to 1.28 | 0.057 to 0.084 | 0.055 to 0.071 | 4325 | 3998 |
| 1024 KiB | 4 | 1.18 to 1.28 | 1.25 to 1.35 | 0.070 to 0.090 | 0.070 to 0.078 | 7340 | 6422 |

### The portable backend (`PosixIo` on `epoll`), reads after the write pass with one data sync at the end of the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.24 to 0.28 | 0.24 to 0.30 | 0.223 to 0.338 | 0.118 to 0.270 | 786 | 754 |
| 64 KiB | 4 | 0.17 to 0.22 | 0.18 to 0.21 | 0.277 to 0.454 | 0.276 to 0.427 | 3015 | 3408 |
| 1024 KiB | 1 | 2.99 to 4.65 | 3.50 to 4.36 | 0.030 to 0.056 | 0.034 to 0.043 | 1114 | 918 |
| 1024 KiB | 4 | 2.49 to 2.66 | 2.29 to 2.47 | 0.050 to 0.057 | 0.058 to 0.061 | 3736 | 3801 |

### The portable backend (`PosixIo` on `epoll`), reads after the write pass with a data sync every 16 writes in the write pass

| Size | Depth | GB/s, file | GB/s, raw | CPU-s a GB, file | CPU-s a GB, raw | p99 µs, file (median of runs) | p99 µs, raw (median of runs) |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1 | 0.27 to 0.31 | 0.29 to 0.31 | 0.099 to 0.224 | 0.117 to 0.148 | 672 | 672 |
| 64 KiB | 4 | 0.16 to 0.23 | 0.19 to 0.21 | 0.236 to 0.433 | 0.321 to 0.338 | 3211 | 3015 |
| 1024 KiB | 1 | 3.75 to 4.62 | 2.85 to 4.30 | 0.029 to 0.041 | 0.030 to 0.057 | 819 | 967 |
| 1024 KiB | 4 | 2.33 to 2.91 | 2.11 to 2.55 | 0.037 to 0.054 | 0.051 to 0.086 | 3277 | 3932 |

### Durable writes (`OpenFlags::with_dsync`), 64 KiB, depth 16

| Backend | GB/s, file | GB/s, raw | p99 µs, file | p99 µs, raw |
|---|---|---|---|---|
| uring | 0.28 to 0.28 | 0.27 to 0.28 | 5243 | 5505 |
| posix | 0.02 to 0.02 | 0.02 to 0.02 | 56623 | 53477 |

## What it shows

- **Through the crate and raw are within a few per cent of each other in
  nearly every cell.** Of 50 cells (backend, size, depth, sync, phase), the
  three runs' GB/s ranges of the two paths are apart in 4: three where the
  set was faster and one where raw was (the portable backend, 64 KiB, depth
  1, a sync every 16 writes: 0.14 to 0.15 against 0.155 to 0.16 GB/s). CPU
  ranges are apart in 5, three with the set lower and two with it higher.
  Three runs a cell do not resolve differences of a few per cent.
- **One cell is faster through the crate than the raw path can explain.**
  The ring, 64 KiB writes at depth 32: 9.21 to 9.74 against 7.86 to 8.92
  GB/s, per run 1.21, 1.09 and 1.09 times, raw first in the second. Both
  paths submit the same `Op::FileWrite`, so the gap lies in the setup (the
  file slot, the order of the passes, the disk's state between them), not
  in the crate; it was not looked into.
- **Depth is what this disk needs.** Reads of 1 MiB reach 13.4 GB/s from
  depth 16; at depth 1 a 64 KiB transfer is about 0.3 GB/s on either path.
- **Writes of 1 MiB stay at 1.8 to 2.6 GB/s** from depth 4, on both paths,
  below what 64 KiB writes reach: the disk's or the file system's behaviour
  with large direct writes to a hot region here, not looked into.
- **CPU on the ring** from depth 4: 0.008 to 0.149 CPU-seconds a GB with
  one sync at the end, up to 0.205 with a sync every 16 writes; most cells
  of 1 MiB transfers lie between 0.01 and 0.05.
- **Syncs every 16 writes** leave 0.74 of the write rate at depth 1 and
  0.32 at depth 32 (64 KiB, the ring, medians); durable writes, each its
  own sync, come to 0.28 GB/s on the ring and 0.02 on the portable backend.
- **The portable backend** reaches 1.5 to 1.8 GB/s writing and 2.3 to
  4.65 GB/s reading with 1 MiB transfers; with 64 KiB, 0.15 to 0.31
  writing and 0.16 to 0.31 reading: operations one at a time.

## Not measured here
Buffered I/O, random offsets, more than one file, a disk being filled, and
the crate's sequences and gathered writes, which `file_bench` does not
drive; this Mac's figures are milestone 36.
