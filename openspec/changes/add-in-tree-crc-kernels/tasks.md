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
- [ ] 4.1 Each kernel against the reference: every length to 4,096 bytes at 16 offsets and to 8,200 across the change of method, every alignment to 64 for lengths to 640, split points; published check values. Done on AArch64 (Apple M3 Max); the x86-64 kernels have been compiled and linted but not run
- [x] 4.2 Hegel property tests and the allocation guard over the new kernels
- [ ] 4.3 Benchmark every kernel against `crc-fast` on Cortex-X925 (DGX Spark) and EPYC 7713, at 64 B, 1 KiB, 64 KiB and 8 MiB; record in `bench/results/`. The benchmark first runs the checks of 4.1 on that machine
- [ ] 4.6 Run the 512-bit kernel on a processor with AVX-512 (Xeon 8358P) before it is relied on
- [ ] 4.4 Set lane counts and the order of preference between 256-bit and 512-bit from 4.3
- [ ] 4.5 End-to-end PUT with checksums against a real store, CPU per GB before and after

## 5. Adoption
- [ ] 5.1 Switch each algorithm that passed 4.3; move `crc-fast` to development dependencies. Switched on the branch ahead of 4.3; to be confirmed or reverted per algorithm by its result
- [ ] 5.2 README and crate documentation: dependencies, supported processors
