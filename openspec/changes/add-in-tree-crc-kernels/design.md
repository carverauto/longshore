## Context
Two functions are on the data path: `crc64_nvme(&[u8])` and
`crc32c(&[u8])`, called once per body. The incremental types are used by a
benchmark tool and by tests. Bodies range from a few bytes to tens of
megabytes.

Measured so far, on an Apple M3 Max only, with a prototype kept outside the
repository: twelve-lane `PMULL` folding beats `crc-fast` by about 8% for
CRC-64/NVME; the same folding is about 20% slower than `crc-fast` for
CRC-32C, because `crc-fast` interleaves the processor's CRC instruction
with `PMULL` there. No x86-64 kernel has been written or measured yet.

## Goals / Non-Goals
- Goals: remove `crc-fast` and its five crates from what ships; be at
  least as fast as it on every benchmark machine; be faster on x86-64
  processors that have 256-bit `VPCLMULQDQ` without AVX-512; cut the cost
  of a call on small bodies
- Non-Goals: other CRC widths or polynomials; combining checksums of
  separate parts; a C interface; a command-line tool; `no_std`

## Decisions
- Decision: choose the kernel at run time, and do not raise the build's
  processor baseline. AVX-512 is not general on x86-64. Intel servers have
  it from Skylake-SP (2017) and `VPCLMULQDQ` from Ice Lake (2021); AMD from
  Zen 4 (2022); Zen 3 and most Intel desktop and laptop processors since
  2021 lack it. A baseline of AVX-512 would stop the crate running on the
  EPYC 7713 it is tested on. The selection is one cached load per call,
  and a build with a raised baseline (`-C target-cpu=...`) turns it into a
  constant with no further code.
  - Alternatives considered: raise the baseline to x86-64-v4 (excludes
    Zen 3); raise it to x86-64-v3 (safe on every benchmark machine, but
    measured no gain on AArch64 from the equivalent change, since kernels
    already compile with their own target features)
- Decision: the fallback below AVX-512 is in-tree, in three steps: 256-bit
  `VPCLMULQDQ`, then 128-bit `PCLMULQDQ`, then tables. The 128-bit step is
  the same algorithm `crc-fast` falls back to.
  - Alternatives considered: keep `crc-fast` as the fallback. It keeps all
    six crates in the shipped tree for a path that is a few dozen lines
    once the wider kernels exist
- Decision: one folding routine per architecture, generic over lane count,
  with CRC-64/NVME and CRC-32C differing only in constants and the final
  reduction. Fold constants are computed from the polynomial in `const`
  functions, not pasted
- Decision: CRC-32C finishes with the processor's CRC instruction (SSE4.2,
  ARMv8 CRC), so every accelerated kernel requires it. No processor with
  `PCLMULQDQ` lacks SSE4.2. Plain folding measured about 22% slower than
  `crc-fast` on an Apple M3 Max, so above 4 KiB the kernel interleaves the
  CRC instruction with the multiplier, as `crc-fast` does: the input is cut
  into one region for the folding lanes and three for the CRC instruction,
  and the four results are joined by multiplication
- Decision: CRC-64/NVME reduces 128 bits to 64 by Barrett reduction; the
  prototype's table step is not kept
- Decision: adoption is per algorithm. If an in-tree kernel is slower than
  `crc-fast` for one algorithm on any benchmark machine after task 4, that
  algorithm stays on `crc-fast` and the result is recorded

## Risks / Trade-offs
- A wrong checksum corrupts nothing silently on S3, which verifies it, but
  fails every upload → every kernel is compared with a bit-at-a-time
  reference on every length to a few thousand bytes and at every
  alignment, each kernel called directly so none is skipped by selection
- A machine without AVX-512 cannot test the 512-bit kernel → the test
  reports which kernels ran; the benchmark task runs on the Ice Lake host
- 512-bit instructions lower the clock on some older Intel processors →
  measured in task 4; the 256-bit kernel is preferred where it is not
  slower
- More `unsafe` in the tree → confined to `x86_64.rs` and `aarch64.rs`,
  with the reads bounded by slice lengths checked before the loop

## Migration Plan
No interface changes. Rollback is restoring the `crc-fast` call in
`lib.rs`.

- The compiler must unroll the lane loops for the lanes to stay in
  registers; one that was not cost 40% → the benchmark names every kernel,
  so a regression of this kind shows as one row falling behind

## Open Questions
- Lane count per processor family: twelve won on Apple silicon; Cortex-X925,
  Ice Lake and Zen 3 are to be measured
- The 512-bit kernel has run on no processor yet. Neither requested
  benchmark machine has AVX-512. Until it passes the tests on one that
  does, it is not proven
