# Milestone 26: sequential direct-I/O throughput against the drive's rating

Measured 2026-11-20 on the DGX Spark (`aarch64`, Linux `7.0.0-1019-nvidia`, ext4) with `taskset -c 19 ./file_uring --dir . --throughput --size-gib 32 --runs 5`, the machine otherwise idle. Raw output: `m26-file-throughput.jsonl`.

One thread keeps 32 operations of 1 MiB in flight over a preallocated 32 GiB file opened with `O_DIRECT`, from registered aligned buffers holding incompressible data. An unmeasured pass writes the whole file first, so the measured writes overwrite written extents. Each run is one write pass, timed up to and including an `fdatasync`, and one read pass. Figures are GB/s (10^9 bytes), median of five runs with the lowest and highest.

The drive is a Samsung PM9E1 4 TB (`MZALC4T0HBL1-00B07`) on a PCIe 5.0 x4 link (`32.0 GT/s`, width 4). Samsung rates the PM9E1 at up to 14.5 GB/s sequential read and 13 GB/s sequential write.

| Pass | GB/s, median (lowest to highest) | Samsung's rating | Share of rating | Client thread |
|---|---|---|---|---|
| Sequential write, with `fdatasync` | 13.13 (13.10 to 13.16) | 13 | 101% | 0.13 |
| Sequential read | 14.87 (14.87 to 14.88) | 14.5 | 103% | 0.15 |

## AWS `i7i.4xlarge`, against fio on the same drive

Measured 2026-11-20 on an `i7i.4xlarge` in us-east-1 (Intel Xeon Platinum 8559C, 16 vCPUs, Ubuntu 24.04, Linux `7.0.0-1017-aws`), on its one 3,750 GB instance-store NVMe drive, formatted XFS minutes before the runs. Same command and shape, pinned to CPU 2. Raw output: `m26-file-throughput-aws-i7i.jsonl`.

fio 3.36 then ran on a 32 GiB file of its own on the same filesystem, laid out first, with `--bs=1M --iodepth=32 --direct=1 --ioengine=io_uring --numjobs=1 --overwrite=1 --end_fsync=1`, also pinned to CPU 2, five runs of each direction. Raw figures: `m26-fio-aws-i7i.txt`.

| Pass | longshore, GB/s median (lowest to highest) | fio, GB/s median (lowest to highest) | longshore thread | fio CPU |
|---|---|---|---|---|
| Sequential write, synced at the end | 3.73 (3.37 to 3.95) | 3.73 (3.46 to 3.73) | 0.01 | 0.10 |
| Sequential read | 4.87 (4.87 to 5.15) | 4.87 (4.87 to 4.87) | 0.07 | 0.13 |

## Write-then-sync latency: the order of the modes was the result

`file_uring --bench` times a 4 KiB `O_DIRECT` write followed by `fdatasync`, as two submissions (`two_ops`) and as one linked chain (`chain`). Until this milestone it measured all of `two_ops` and then all of `chain`. Milestone 2 and the final summary report the chain as 16 to 23% faster at the median and about 50% faster at p95 on the DGX Spark from that order.

`--chain-first` swaps the order and `--interleave` takes ten of each in turn. Raw output for every run: `m26-file-bench-order-dgx-spark.txt` and `m26-file-bench-order-aws-i7i.txt`. Three runs of 200 per order on each machine, in microseconds, median of the three runs:

| Machine | Order | `two_ops` p50 | `chain` p50 | `two_ops` p95 | `chain` p95 |
|---|---|---|---|---|---|
| DGX Spark | `two_ops` first | 2046.7 | 1622.7 | 5041.1 | 2822.1 |
| DGX Spark | `chain` first | 1534.6 | 2031.7 | 2633.4 | 5172.2 |
| AWS `i7i.4xlarge` | `two_ops` first | 20.8 | 20.6 | 127.8 | 28.6 |
| AWS `i7i.4xlarge` | `chain` first | 20.4 | 20.8 | 29.5 | 128.2 |

Whichever mode runs first is slower, on both machines.

Interleaved, 2,000 of each mode: one run on the DGX Spark, the median of three on AWS.

| Machine | Mode | p50 | p95 | p99 | mean |
|---|---|---|---|---|---|
| DGX Spark | `two_ops` | 1476.9 | 2625.2 | 3127.1 | 1562.6 |
| DGX Spark | `chain` | 1309.3 | 2575.0 | 3236.0 | 1506.2 |
| AWS `i7i.4xlarge` | `two_ops` | 20.4 | 24.9 | 33.1 | 21.0 |
| AWS `i7i.4xlarge` | `chain` | 20.4 | 24.9 | 34.5 | 21.0 |

Three interleaved runs of 200 on the DGX Spark put the chain's median 1.3%, 3.3% and 6.9% below `two_ops`.

## What the figures say

- On the DGX Spark one thread moves what the drive is rated for in both directions while about 85% idle. The drive is the limit, not the client.
- On the AWS instance longshore and fio have the same medians, and read runs repeat to the hundredth of a GB/s, which looks like a rate the instance sets for the drive. longshore's thread used less CPU than fio's process.
- The latency gain reported for the linked chain in milestone 2 and the final summary was the order of measurement. Interleaved, the chain's median is 1 to 11% lower on the DGX Spark, where the sync takes over a millisecond, with the same tails. On the AWS drive the pair takes 20 microseconds and the modes do not differ.
- Both modes report `enters_per_op` of 2.00: the chain is one submission, but the benchmark still waits twice, once for each completion.
- The link's own ceiling is about 15.8 GB/s (32 GT/s on four lanes with 128b/130b encoding), so the read pass is within 6% of the wire.

## Limits of this measurement

- One drive, one filesystem and one shape (1 MiB, depth 32). No other block size or depth was run.
- A 32 GiB write may fit in the drive's write cache. A longer write was not run, so this is not a sustained-write figure.
- On the DGX Spark no other tool was run for comparison. The rating is Samsung's published figure, not a measurement.
- fio ran after longshore on a different file, not interleaved with it.
- Why the first mode measured is slower was not investigated. Twenty warm-up operations of each mode run before it.
- The x86 host of milestone 2 was not measured: its test filesystem is on a RAID controller, not a single NVMe drive, and the host was busy. Its latency rows in milestone 2 and the final summary are from that volume, in the fixed order.
