# Milestone 26: sequential direct-I/O throughput against the drive's rating

Measured 2026-11-20 on the DGX Spark (`aarch64`, Linux `7.0.0-1019-nvidia`, ext4) with `taskset -c 19 ./file_uring --dir . --throughput --size-gib 32 --runs 5`, the machine otherwise idle. Raw output: `m26-file-throughput.jsonl`.

One thread keeps 32 operations of 1 MiB in flight over a preallocated 32 GiB file opened with `O_DIRECT`, from registered aligned buffers holding incompressible data. An unmeasured pass writes the whole file first, so the measured writes overwrite written extents. Each run is one write pass, timed up to and including an `fdatasync`, and one read pass. Figures are GB/s (10^9 bytes), median of five runs with the lowest and highest.

The drive is a Samsung PM9E1 4 TB (`MZALC4T0HBL1-00B07`) on a PCIe 5.0 x4 link (`32.0 GT/s`, width 4). Samsung rates the PM9E1 at up to 14.5 GB/s sequential read and 13 GB/s sequential write.

| Pass | GB/s, median (lowest to highest) | Samsung's rating | Share of rating | Client thread |
|---|---|---|---|---|
| Sequential write, with `fdatasync` | 13.13 (13.10 to 13.16) | 13 | 101% | 0.13 |
| Sequential read | 14.87 (14.87 to 14.88) | 14.5 | 103% | 0.15 |

## What the figures say

- One thread moves what the drive is rated for in both directions while about 85% idle. The drive is the limit, not the client.
- The link's own ceiling is about 15.8 GB/s (32 GT/s on four lanes with 128b/130b encoding), so the read pass is within 6% of the wire.

## Limits of this measurement

- One drive, one filesystem and one shape (1 MiB, depth 32). No other block size or depth was run.
- A 32 GiB write may fit in the drive's write cache. A longer write was not run, so this is not a sustained-write figure.
- No other tool was run on the same file for comparison. The rating is Samsung's published figure, not a measurement.
- The x86 test host was not measured: its test filesystem is on a RAID controller, not a single NVMe drive, and the host was busy.
