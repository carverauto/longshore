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
- [x] 4.1 Each kernel against the reference: every length to 4,096 bytes at 16 offsets and to 8,200 across the change of method, every alignment to 64 for lengths to 640, split points; published check values. Run on thirteen processors, x86-64 and AArch64 (`bench/results/m23-crc-kernels.md`)
- [x] 4.2 Hegel property tests and the allocation guard over the new kernels
- [x] 4.3 Benchmark every kernel against `crc-fast` at 64 B, 1 KiB, 64 KiB and 8 MiB; record in `bench/results/`. Done on eleven processors at commit `dffc639`: `bench/results/m23-crc-kernels.md`. The gate is not met for either checksum
- [x] 4.6 Run the 512-bit kernel on a processor with AVX-512: passed on five
- [ ] 4.7 Repeat 4.1 and 4.3 after the changes made in answer to the first results (`bench/results/m23-crc-kernels.md`); the 256-bit and 512-bit kernels have not run since they were rewritten
- [ ] 4.8 From 4.7: confirm the interleaved 512-bit CRC-32C on Sapphire Rapids, choose the lane count for Neoverse cores, and decide whether the 256-bit tier interleaves
- [ ] 4.4 Set lane counts and the order of preference between 256-bit and 512-bit from 4.3
- [ ] 4.5 End-to-end PUT with checksums against a real store, CPU per GB before and after

## 5. Adoption
- [ ] 5.1 Switch each algorithm that passes 4.3 and 4.7; move `crc-fast` to development dependencies. Switched on the branch ahead of the gate. On the first results neither checksum passes, so merging as it stands would be against this change's own rule
- [ ] 5.2 README and crate documentation: dependencies, supported processors
