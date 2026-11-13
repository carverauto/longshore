# Milestone 5 — checksum library comparison

Measured 2026-11-12 with `cargo bench -p osc-bench --bench checksums` (criterion), one core, GiB/s (2^30 bytes). Every candidate first had to reproduce the published check value for its algorithm.

Reference from the talk: 4.3 GB/s for their optimized CRC64, 0.5 GB/s for the Zig standard library.

## CRC-64/NVME (AWS, Azure)

| Library | arm64 Cortex-X925, pinned | x86-64 Xeon 8358P (Ice Lake), shared host | x86-64 EPYC 7713 (Zen 3), dedicated vCPU |
|---|---|---|---|
| `crc` (table lookup) | 0.52 | 0.37 | 0.36 |
| `crc64fast-nvme` | 59.8 | 23.2 | 11.3 |
| `crc-fast` | 81.9 | 31.3 | 11.3 |

## CRC-32C (Google Cloud Storage)

| Library | arm64 Cortex-X925 | x86-64 Xeon 8358P | x86-64 EPYC 7713 |
|---|---|---|---|
| `crc` (table lookup) | 0.53 | 0.37 | 0.36 |
| `crc32c` | 19.5 | 22.2 | 21.6 |
| `crc-fast` | 85.3 | 31.3 | 16.7 |

All figures are for 8 MiB inputs; 64 KiB results are within about 40% of these and are in the criterion output.

## Decision

`crc-fast` is adopted for both algorithms, behind `osc-checksum`.

- It exceeds the 4.3 GB/s reference on all three machines: by about 2.8x on the slowest (EPYC 7713) and about 20x on the fastest.
- It works on slices in place, so it adds no payload copy, and a test runs it with heap allocation sealed.
- It covers both algorithms with one dependency.
- On Zen 3 the `crc32c` crate is faster for CRC-32C (21.6 against 16.7). That is not worth a second dependency now; revisit if GCS on AMD becomes a target.

No in-tree CRC implementation is needed.

## End-to-end cost on the PUT path

`s3_uring put` against `put-crc` from host A to the Linode bucket, 8 connections, 8 MiB objects, 10 s, two runs each. The link is slow (about 80 MB/s), so throughput means nothing here; CPU time per byte does.

| Mode | Thread CPU per GB stored |
|---|---|
| No checksum | 47 ms, 50 ms |
| CRC-64/NVME computed by the client | 83 ms, 81 ms |

The checksum adds about 33 ms of CPU per GB, which is roughly 30 GB/s of CRC throughput in the real path. That is about a third of the microbenchmark's 88 GB/s, as expected when each 8 MiB buffer is cold in the cache, and still about seven times the talk's 4.3 GB/s. It answers the caveat below about the arm64 figure: the microbenchmark number is real but is a warm-cache best case.

Linode accepted every checksummed store. It does not verify the value (see `m4-s3-linode-us-ord.md`), so this confirms the header is well-formed and signed correctly, not that a store enforces it.

## Caveats

- The arm64 figures are far above what I expected. Two independent libraries agree to within 40%, and `crc-fast` matches a bit-at-a-time reference on every input the property tests generate and on a 1 MB buffer, so the results are correct; the speed should still be confirmed by an end-to-end PUT with checksums on.
- The Xeon host was running VMs and the thread was not pinned.
- The talk does not say what CPU its 4.3 GB/s was measured on.
