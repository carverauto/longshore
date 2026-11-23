## 1. Shared parts
- [x] 1.1 Fold constants from the polynomial in `const` functions; table kernel; bit-at-a-time reference for tests
- [x] 1.2 Kernel selection: run-time detection, constant when the build's baseline already has the feature
- [x] 1.3 Barrett reduction for CRC-64/NVME

## 2. AArch64
- [x] 2.1 `PMULL` kernel for both algorithms, with and without `EOR3`
- [x] 2.2 CRC-32C: interleave the CRC instruction if plain folding is slower than `crc-fast` (it was, on Apple M3 Max)

## 3. x86-64
- [x] 3.1 128-bit `PCLMULQDQ` kernel
- [x] 3.2 256-bit `VPCLMULQDQ` kernel (AVX2)
- [x] 3.3 512-bit `VPCLMULQDQ` kernel (AVX-512)
- [x] 3.4 CRC-32C finish with the SSE4.2 CRC instruction

## 4. Proof
- [x] 4.1 Each kernel against the reference: every length to 4,096 bytes at 16 offsets and to 8,200 across the change of method, every alignment to 64 for lengths to 640, split points; published check values. Run on thirteen processors, x86-64 and AArch64 (`bench/results/m24-crc-kernels.md`)
- [x] 4.2 Hegel property tests and the allocation guard over the new kernels
- [x] 4.3 Benchmark every kernel against `crc-fast` at 64 B, 256 B, 1 KiB, 64 KiB and 8 MiB; record in `bench/results/`. Two runs, the second on thirteen machines: `bench/results/m24-crc-kernels.md`
- [x] 4.6 Run the 512-bit kernel on a processor with AVX-512: passed on five
- [x] 4.7 Repeat 4.1 and 4.3 after the changes made in answer to the first results. Done at commit `f176238`: every kernel passes on every machine, the rewritten 256-bit and 512-bit ones included
- [x] 4.8 Decide on what remains behind `crc-fast`: adopted, with the cells tracked in issue #29
- [x] 4.4 Set lane counts and the order of preference between 256-bit and 512-bit from the runs. Set from the first run; choosing the lane count by length on Arm is in issue #29
- [x] 4.5 End-to-end PUT with checksums against a real store, CPU per GB before and after. S3 accepted 18,689 checksummed uploads from the branch (`m24-crc-kernels.md`). Amazon S3 PUT with CRC-64/NVME, same instances, kernel, pinning and 384 connections, client thread at 1.00 core in both: before, `crc-fast` (`m20-preference-and-comparison.md`), 16.4 GB/s on Graviton4 and 13.3 on Zen 5, about 61 and 75 ms of thread CPU per GB; after, the in-tree kernels (`m26-s3-aws-large-instances-current.md`), 16.7 and 14.1 GB/s, about 60 and 71 ms per GB. The after run is also on the ring-per-core branch and on another day, so the difference is not attributed to the kernels alone; it shows no regression

## 5. Adoption
- [x] 5.1 Both algorithms switched; `crc-fast` is a development dependency of the benchmark only
- [x] 5.2 README and crate documentation: dependencies, supported processors
