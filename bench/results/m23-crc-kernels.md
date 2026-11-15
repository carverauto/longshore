# Milestone 23 — in-tree CRC kernels against `crc-fast`

Measured 2026-11-14 by a colleague on the branch `feat/in-tree-crc-kernels` at commit `dffc639`, and reported in https://github.com/carverauto/longshore/issues/6 (comments 6888495624). `cargo bench -p longshore-bench --bench checksums` built for `*-unknown-linux-musl` (criterion, 1 s warm-up, 3 s measurement). The 1 KiB and 64 KiB cells were run twice more. The raw criterion output is kept by the person who ran it and is not in this repository.

This is the comparison the change `add-in-tree-crc-kernels` asks for before an algorithm moves to its in-tree kernel.

## Correctness

On each machine: the crate's kernel test (every kernel called directly against the bit-at-a-time reference), and a guard-page test written for the run, which places each buffer against an unreadable page so a read outside the slice faults.

| Machine | Processor | Kernels that ran | Kernel test | Guard-page test |
|---|---|---|---|---|
| AWS m8a.large | AMD Zen 5 (EPYC 9R45) | all 9, 512-bit included | pass | pass |
| AWS c7a.large | AMD Zen 4 (EPYC 9R14) | all 9 | pass | pass |
| AWS c8i.large | Intel Granite Rapids (Xeon 6975P-C) | all 9 | pass | pass |
| AWS c7i.large | Intel Sapphire Rapids (8488C) | all 9 | pass | pass |
| AWS c6i.large | Intel Ice Lake (8375C) | all 9 | pass | pass |
| local x86-64 | Intel Ice Lake (8358P) | all 9 | pass | not run |
| AWS c6a.large | AMD Zen 3 (EPYC 7R13) | 7: 256-bit and 128-bit | pass | pass |
| AWS c5.large | Intel Cascade Lake (8275CL) | 5: 128-bit only | pass | pass |
| AWS c8g.large | Graviton4 (Neoverse V2) | all 12 | pass | pass |
| AWS c7g.large | Graviton3 (Neoverse V1) | all 12 | pass | pass |
| AWS c6g.large | Graviton2 (Neoverse N1) | 6: no EOR3 | pass | pass |
| local Arm | Cortex-X925 | all 12 | pass | not run |
| Apple M-series (macOS) | | all 12 | pass | pass |

The 512-bit kernel ran and passed on five processors. Cascade Lake has AVX-512 without `VPCLMULQDQ`; the 128-bit kernel is selected there, as intended.

Uploads to Amazon S3 over HTTPS with checksums on, objects of 8 MiB, 1,000,003 bytes and 65,537 bytes: 7,518 on Zen 5 (512-bit kernel selected) and 7,194 on Graviton4. S3 verifies the CRC-64/NVME and refuses a wrong one. None failed.

## Speed: the crate as selected, relative to `crc-fast` 1.10

Throughput of the branch divided by that of `crc-fast`. Above 1 the branch is faster.

CRC-64/NVME:

| Processor | 64 B | 1 KiB | 64 KiB | 8 MiB |
|---|---|---|---|---|
| Zen 5 | 2.91 | 1.06 | 1.00 | 1.00 |
| Zen 4 | 2.23 | 1.24 | 1.00 | 1.00 |
| Zen 3 | 2.38 | 2.07 | 2.00 | 1.96 |
| Granite Rapids | 3.30 | 1.98 | 1.38 | 1.00 |
| Sapphire Rapids | 3.20 | 2.08 | 1.03 (reruns) | 1.00 |
| Ice Lake (AWS) | 2.96 | 1.38 | 1.02 | 0.99 |
| Cascade Lake | 2.34 | 1.05 | 1.16 | 1.09 |
| Graviton4 | 4.63 | 0.78 | 0.97 | 0.98 |
| Graviton3 | 4.54 | 0.76 | 0.94 | 0.98 |
| Graviton2 | 2.07 | 0.85 | 1.03 | 1.04 |
| Cortex-X925 | 5.92 | 0.92 | 1.20 | 1.07 |

CRC-32C:

| Processor | 64 B | 1 KiB | 64 KiB | 8 MiB |
|---|---|---|---|---|
| Zen 5 | 1.11 | 0.69 | 0.99 | 1.00 |
| Zen 4 | 1.16 | 1.17 | 1.01 | 1.00 |
| Zen 3 | 1.74 | 2.21 | 1.45 | 1.40 |
| Granite Rapids | 2.27 | 1.17 | 1.00 | 1.00 |
| Sapphire Rapids | 1.91 | 1.44 | 0.74 | 1.02 |
| Ice Lake (AWS) | 1.99 | 0.98 | 1.03 | 1.01 |
| Cascade Lake | 1.20 | 1.27 | 0.60 | 0.93 |
| Graviton4 | 1.15 | 0.93 | 1.10 | 1.03 |
| Graviton3 | 1.13 | 0.88 | 1.10 | 1.02 |
| Graviton2 | 0.94 | 0.82 | 1.40 | 1.37 |
| Cortex-X925 | 1.33 | 1.24 | 1.13 | 1.01 |

The cells below 0.95 repeated in the two extra runs to within 0.03. One did not: Sapphire Rapids CRC-64 at 64 KiB read 0.81 first and 1.03 in both reruns, because `crc-fast` itself read 80 and then 64 GiB/s.

At 8 MiB most x86 machines read about 27 GiB/s for every kernel and for `crc-fast`: the memory of a `.large` instance, not the kernel.

Per-kernel figures the report gives, in GiB/s:

| Processor, checksum, size | Selected | Best kernel in the build | `crc-fast` |
|---|---|---|---|
| Cascade Lake, CRC-32C, 64 KiB | 26.2 (`sse-pclmulqdq x8`) | 42.8 (`sse-pclmulqdq x4 +3x3`) | 43.8 |
| Graviton4, CRC-32C, 1 KiB | 20.3 | 27.8 (`pmull-eor3 x9 +3x2`) | 21.9 |
| Graviton3, CRC-32C, 1 KiB | 17.8 | 23.6 (`pmull-eor3 x9 +3x2`) | 20.3 |
| Graviton4, CRC-64, 1 KiB | | 18.0 | 22.7 |
| Graviton3, CRC-64, 1 KiB | | 15.9 | 20.6 |
| Sapphire Rapids, CRC-32C, 64 KiB | | 69.9 | 87 to 92 |
| Zen 5, CRC-32C, 1 KiB | | 32.6 | 43 to 47 |

## Decision

Not adopted on these figures. The change's rule is that an algorithm moves to its in-tree kernel only when it is at least as fast as `crc-fast` on every benchmark machine at 64 KiB and 8 MiB. At those sizes:

- CRC-64/NVME is behind on Graviton3 (0.94 and 0.98) and Graviton4 (0.97 and 0.98), and at 0.99 on Ice Lake at 8 MiB.
- CRC-32C is behind on Sapphire Rapids (0.74 at 64 KiB) and Cascade Lake (0.60 and 0.93).

What the figures do establish: small bodies are 2 to 6 times faster for CRC-64 on every machine, and Zen 3, which `crc-fast` serves with its 128-bit kernel, is twice as fast at every size.

## Changes made in answer, not yet measured on these machines

1. **Registers are collapsed side by side.** After the main loop the lanes were folded one into the next: eleven multiplications in a row for twelve lanes, a fixed cost on every call that loops. Each is now moved to the last one's place by a constant of its own, so the multiplications do not wait for one another. The same for the blocks inside a 256-bit or 512-bit register.
2. **What the main loop leaves is taken eight blocks at a time, then four,** in the same way, and only the last three or fewer one after another. Before, up to seven blocks after a four-lane stage, and every block of an input too short for the main loop, were folded in a chain.
3. **CRC-32C interleaves the CRC instruction with 512-bit registers** on processors that have them, for inputs past about 5 KiB: four registers and three streams of three words a round, the proportions `crc-fast` uses there. This is aimed at Sapphire Rapids at 64 KiB (0.74).
4. **Processors with the 128-bit multiply only** use four lanes with three CRC streams for CRC-32C on inputs past about 2 KiB, the kernel the Cascade Lake row shows at 42.8 against the selected 26.2.
5. **One routine serves every register width.** The main loop, the collapse and the interleaved kernel are written once against a register of one, two or four blocks. The AArch64 and 128-bit runs therefore exercise the code the wider kernels share; what is particular to a 256-bit or 512-bit register is five small functions.

Kernels added to the benchmark for measurement, not selected: interleaved CRC-32C on 256-bit registers (`avx2-vpclmulqdq x8 +3x3`, `x4 +3x2`), a second proportion on 512-bit ones (`x4 +3x6`), and four and six lanes on AArch64. The benchmark also has a 256-byte size now.

On an Apple M3 Max, the crate as selected against `crc-fast`, after these changes (GiB/s):

| | 64 B | 256 B | 1 KiB | 64 KiB | 8 MiB |
|---|---|---|---|---|---|
| CRC-64/NVME | 12.8 against 3.8 | 27.6 against 22.1 | 49.2 against 45.7 | 69.1 against 65.9 | 70.8 against 64.3 |
| CRC-32C | 13.5 against 11.4 | 34.4 against 27.7 | 56.0 against 52.6 | 92.2 against 83.4 | 91.7 against 91.3 |

That processor is not one longshore runs on and hides most of a serial chain, so the Neoverse cores should gain more at 1 KiB than it does.

The 256-bit and 512-bit kernels were rewritten by (1), (3) and (5) after the runs above, and no processor that has them has run them since. All tests pass on AArch64 and, for the 128-bit kernels, on x86-64.

## Still open

- Whether (3) closes Sapphire Rapids at 64 KiB, and what it does on Zen 4, Zen 5, Granite Rapids and Ice Lake, where plain folding was already level.
- Graviton3 and Graviton4 at 64 KiB and 8 MiB for CRC-64 were 2 to 6% behind with twelve lanes. The rerun's rows for four, six and eight lanes settle the lane count there.
- Whether the 256-bit tier (Zen 3) should use an interleaved CRC-32C: both proportions are in the benchmark.

## Caveats

- One run per cell, three for 1 KiB and 64 KiB. `.large` instances share their memory bandwidth and their processors with other tenants.
- The run was of commit `dffc639`. Nothing here measures the changes above on those machines.
- Processors older than Cascade Lake and Zen 3, and targets served by the table kernel, were not run.
