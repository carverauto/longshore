## Why
`longshore_crc` wraps `crc-fast` and uses two of its functions: a one-shot
CRC-64/NVME and a one-shot CRC-32C. The dependency brings five more crates
(`digest`, `crypto-common`, `generic-array`, `typenum`, `spin`) and about
70,000 lines for every CRC width and parameter set it supports.

Its kernels also leave throughput unused on machines longshore runs on:

- On x86-64 it uses the wide carry-less multiply (`VPCLMULQDQ`) only when
  AVX-512 is present. A processor with the 256-bit form and no AVX-512
  (AMD Zen 3) gets the 128-bit kernel: 11.3 GiB/s for CRC-64/NVME on an
  EPYC 7713, where the `crc32c` crate reaches 21.6 GiB/s for CRC-32C
  (`bench/results/m5-checksums.md`).
- On AArch64 it folds eight lanes. A prototype folding twelve measured
  about 8% faster for CRC-64/NVME on an Apple M3 Max (49.1 against
  45.4 GiB/s at 8 MiB).
- Each call costs about 20 ns of set-up and dispatch. The prototype took
  5 ns for a 64-byte input against 21 ns.

## What Changes
- CRC-64/NVME and CRC-32C are computed by kernels in `longshore_crc`
- x86-64 kernels: 512-bit `VPCLMULQDQ` with AVX-512, 256-bit `VPCLMULQDQ`
  with AVX2, 128-bit `PCLMULQDQ` with SSE4.2
- AArch64 kernel: `PMULL`, using `EOR3` where the processor has it. For
  CRC-32C the processor's CRC instruction reads part of the input beside it
- A table kernel for every other processor and target
- The kernel is chosen when the program runs, from what the processor
  reports. The build's processor baseline is not raised
- `crc-fast` leaves the shipped dependency tree. It stays a development
  dependency of the benchmark, to measure against. The tests compare with
  a bit-at-a-time reference instead
- The public functions and types of `longshore_crc` do not change

## Impact
- Affected specs: `checksum-kernels` (new)
- Affected code: `crates/longshore_crc` (new kernel modules, tests),
  `Cargo.toml`, `crates/longshore-bench/benches/checksums.rs`,
  `bench/results/`, README dependency notes
- First principles: 2 holds, the kernels use no heap and the selection
  reads a value the standard library caches. 3 and 4 are unchanged. 7 is
  the gate: an algorithm moves to the in-tree kernel only on the
  benchmark in task 4
- New `unsafe` code: processor intrinsics, confined to one module per
  architecture
