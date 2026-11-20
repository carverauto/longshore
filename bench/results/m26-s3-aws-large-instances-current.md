# Milestone 26: Amazon S3 from the large instances, on the ring-per-core branch

Measured 2026-11-19 with `s3_uring` built from `feat/ring-per-core` at commit `a1ce173`, against a general-purpose S3 bucket in us-east-1, from two instances in the same region, one at a time:

- `c8gn.24xlarge`: Graviton4 (Neoverse V2), 96 vCPUs, one NUMA node, 300 Gbit/s network card. Client thread pinned to CPU 2.
- `m8azn.24xlarge`: AMD EPYC 9R05 (Zen 5), 96 vCPUs, two NUMA nodes, 200 Gbit/s network card. Client thread pinned to CPU 50, on the card's NUMA node and not one of the cores that take its interrupts.

Both ran Ubuntu 24.04 with Linux `7.0.0-1017-aws` and the ENA driver, which does not segment in hardware (`tx-tcp-segmentation: off [fixed]`). MTU 9001. One client io_uring thread, 8 MiB objects, 8 s per run, five runs per cell. GB = 10^9 bytes. `S3_PREFER_LARGE_SEGMENTS=2` and `S3_REQUIRE_CHECKSUMS=1` throughout. The HTTPS rows use the benchmark's default send batch. The client used the instance role's temporary keys.

"Whole machine" is cores busy across the instance, from `/proc/stat` read just before and just after each run. That window includes the client's start-up and connection set-up as well as the 8 s that are measured. Raw output: `m26-s3-aws-large-instances-current.jsonl`, where each line names its instance, cell and run.

These are the same instance types, kernel, pinning and connection counts as the rows with the preference on in milestone 20, so the two can be read side by side. Milestone 20 predates the in-tree CRC kernels and the ring-per-core changes.

## Correctness

The self-test passed over HTTP and HTTPS on both instances before the runs, with `allocation_sealed=true` on all four.

One request out of 592,387 across both instances needed a second attempt. It was in an HTTPS PUT run on the Graviton4 instance.

## Graviton4, 300 Gbit/s card

Median, with the lowest and highest in brackets. Every column is the median of that column over the runs, so the figures on a row are not from one run.

| Request | Conns | GB/s | Client thread | Whole machine, cores busy |
|---|---|---|---|---|
| PUT | 640 | 36.4 (36.2 to 36.6) | 0.54 | 3.3 |
| PUT with CRC-64/NVME | 384 | 16.7 (16.6 to 17.1) | 1.00 | 1.9 |
| GET | 128 | 4.6 (4.5 to 4.8) | 1.00 | 1.1 |
| HTTPS PUT | 128 | 5.1 (3.9 to 5.2) | 0.99 | 1.2 |
| HTTPS GET | 128 | 2.8 (2.7 to 2.9) | 1.00 | 1.1 |

## AMD Zen 5, 200 Gbit/s card

| Request | Conns | GB/s | Client thread | Whole machine, cores busy |
|---|---|---|---|---|
| PUT | 384 | 23.9 (23.7 to 24.1) | 0.64 | 3.2 |
| PUT with CRC-64/NVME | 384 | 14.1 (13.9 to 14.7) | 1.00 | 2.4 |
| GET | 128 | 7.9 (7.8 to 7.9) | 1.00 | 1.2 |
| HTTPS PUT | 128 | 7.6 (7.4 to 7.7) | 0.99 | 1.4 |
| HTTPS GET | 128 | 5.2 (5.2 to 5.2) | 0.99 | 1.2 |

## Next to milestone 20

Medians, preference on in both. Milestone 20's HTTPS PUT figure is its 3 MiB batch row.

| Instance | Request | Milestone 20 | Here |
|---|---|---|---|
| Graviton4 | PUT, 640 conns | 36.7 | 36.4 |
| Graviton4 | PUT with CRC-64/NVME | 16.4 | 16.7 |
| Graviton4 | GET | 4.4 | 4.6 |
| Graviton4 | HTTPS PUT | 5.2 | 5.1 |
| Graviton4 | HTTPS GET | 2.7 | 2.8 |
| AMD | PUT, 384 conns | 22.4 | 23.9 |
| AMD | PUT with CRC-64/NVME | 13.3 | 14.1 |
| AMD | GET | 7.9 | 7.9 |
| AMD | HTTPS PUT | 7.3 | 7.6 |
| AMD | HTTPS GET | 5.1 | 5.2 |

Reading it:

- Plain PUT on Graviton4 still fills the 300 Gbit/s card, at about 291 Gbit/s, with the client's thread a little under half idle. The five runs agree within 1%.
- Nothing is slower than milestone 20 by more than the spread between runs. The largest differences are on the AMD instance: plain PUT is 7% higher and PUT with a checksum 6% higher. Two sets of five runs on different days do not show whether that is the code or the day.
- One Graviton4 HTTPS PUT run gave 3.9 GB/s against 5.1 to 5.2 for the other four. It retired 127 small-segment connections, the most of any run in that cell, and is the run with the retried request.

## CRC-64/NVME alone, on the DGX Spark

Taken the same day from the same commit, with `cargo bench -p longshore-bench --bench checksums` built for `aarch64-unknown-linux-musl` (criterion, 1 s warm-up, 3 s measurement), pinned to Cortex-X925 CPU 19 with `taskset`. 8 MiB input, three runs, GiB/s (2^30 bytes) as criterion reports it.

| | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| `longshore` as selected (`pmull-eor3 x12 +3x3`) | 82.5 | 80.3 | 82.5 | 82.5 |
| `crc-fast` 1.10 | 75.3 | 79.0 | 79.4 | 79.0 |

The crate's median is 88.5 GB/s (10^9 bytes), from 86.2 to 88.6. Milestone 24 has the ratio to `crc-fast` on this core but no absolute figure. The criterion output was not kept.

## Caveats

- One afternoon, one region, one bucket, five runs of 8 s per cell.
- The preference was on for every run. The same cells with it off were not repeated; milestone 20 has them.
- The whole-machine figures are lower than milestone 20's for the same cells (3.3 against 3.9 for Graviton4 plain PUT). Milestone 20 does not say how its window was taken, so the two columns may not be measured the same way.
- The bucket and the instances were deleted afterwards.
