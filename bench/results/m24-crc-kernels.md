# Milestone 24 — in-tree CRC kernels against `crc-fast`

Two runs by a colleague on the branch `feat/in-tree-crc-kernels`, with `cargo bench -p longshore-bench --bench checksums` built for `*-unknown-linux-musl` (criterion, 1 s warm-up, 3 s measurement):

- the first on 2026-11-14 at commit `dffc639`, reported in https://github.com/carverauto/longshore/issues/6
- the second at commit `f176238`, after the changes the first led to, reported in https://github.com/carverauto/longshore/issues/29

The raw criterion output is kept by the person who ran it and is not in this repository.

## Decision

**Both checksums are computed in-tree.** The kernels are correct on every machine tried, faster than `crc-fast` on most cells and much faster on small bodies and on Zen 3. Some cells remain behind. The owner decided to adopt with those gaps tracked in issue #29 and not to hold the change for them; none is on a machine or at a size that matters to longshore at this stage. The second run's tables below say where they are.

The rest of this file is the record: the second run first, then the first run as history.

## Second run (commit `f176238`)

### Tests

All pass on every machine.

| Check | Machines | Result |
|---|---|---|
| The crate's kernel test, each kernel called directly against the bit-at-a-time reference | 10 AWS instance types, 2 local Linux hosts, 1 Mac | pass on all 13 |
| Guard-page test: each buffer against an unreadable page on either side, so a read outside the slice faults | the same 13 | pass on all 13 |
| `scripts/test.sh`: the workspace tests and the real-kernel checks on Linux 6.8 x86-64 and 6.14 Arm | Mac and 2 local Linux hosts | 515 tests pass, real-kernel checks pass on both hosts |
| Uploads to Amazon S3 over HTTPS with checksums on, objects of 8 MiB, 1,000,003, 65,537 and 8,193 bytes | Zen 5 (512-bit kernel selected) and Graviton4 | 18,689 uploads, none refused |

Every kernel the processor supports ran on each machine: 14 on the six AVX-512 machines, 10 on Zen 3, 6 on Cascade Lake, 16 on Graviton3, Graviton4, Cortex-X925 and Apple silicon, 6 on Graviton2. The 256-bit and 512-bit kernels, rewritten after the first run, are among them.

### Speed relative to `crc-fast` 1.10

Throughput of the crate as it selects, divided by `crc-fast`. Above 1 the crate is faster. Median of three runs, with the lowest and highest in brackets where they differ by more than 0.03. AWS machines are `.large` instances. The local Ice Lake host runs other workloads.

CRC-64/NVME:

| Processor | 64 B | 256 B | 1 KiB | 64 KiB | 8 MiB |
|---|---|---|---|---|---|
| Zen 5 (EPYC 9R45) | 2.75 [2.74 to 2.96] | 1.56 [1.54 to 1.62] | 1.08 [1.06 to 1.15] | 1.01 | 1.00 |
| Zen 4 (EPYC 9R14) | 2.19 [2.18 to 2.44] | 1.46 [1.45 to 1.57] | 1.23 [1.23 to 1.31] | 1.01 | 1.00 |
| Zen 3 (EPYC 7R13) | 2.38 | 1.71 | 2.06 | 2.00 | 1.96 |
| Granite Rapids (Xeon 6975P-C) | 3.00 [2.98 to 3.35] | 2.53 [2.52 to 2.83] | 1.73 [1.72 to 1.89] | 1.03 [1.03 to 1.36] | 1.00 |
| Sapphire Rapids (8488C) | 2.88 [1.87 to 2.89] | 2.51 | 1.72 | 1.27 [0.79 to 1.28] | 1.01 |
| Ice Lake (8375C) | 2.92 | 2.47 | 1.54 | 1.03 | 0.98 |
| Ice Lake (8358P), local, shared host | 2.70 [2.05 to 2.84] | 1.72 [1.62 to 2.17] | 1.42 [1.38 to 1.50] | 1.05 [1.02 to 1.20] | 1.00 [0.88 to 1.15] |
| Cascade Lake (8275CL) | 2.48 | 1.69 [1.59 to 1.69] | 1.36 | 1.17 | 1.07 |
| Graviton4 (Neoverse V2) | 3.94 [3.93 to 4.00] | 1.04 | 0.94 | 0.98 | 0.97 |
| Graviton3 (Neoverse V1) | 3.57 [3.56 to 3.62] | 0.92 | 0.93 | 0.96 | 0.97 |
| Graviton2 (Neoverse N1) | 1.73 | 0.93 | 1.03 | 1.04 | 1.03 |
| Cortex-X925, local | 5.06 [5.06 to 5.11] | 0.91 | 0.90 | 1.23 | 1.07 [1.04 to 1.15] |

CRC-32C:

| Processor | 64 B | 256 B | 1 KiB | 64 KiB | 8 MiB |
|---|---|---|---|---|---|
| Zen 5 (EPYC 9R45) | 1.14 [1.14 to 1.53] | 0.67 [0.67 to 0.78] | 0.67 [0.67 to 0.72] | 1.24 | 1.15 |
| Zen 4 (EPYC 9R14) | 1.13 [1.11 to 1.49] | 1.10 | 1.10 [1.08 to 1.13] | 1.18 | 1.19 |
| Zen 3 (EPYC 7R13) | 1.66 | 1.30 | 2.13 | 1.40 | 1.35 |
| Granite Rapids (Xeon 6975P-C) | 1.88 | 1.42 | 1.00 [0.99 to 1.06] | 0.73 [0.72 to 0.97] | 1.00 |
| Sapphire Rapids (8488C) | 1.70 [1.37 to 1.73] | 1.30 | 0.97 | 0.92 [0.69 to 1.04] | 1.03 |
| Ice Lake (8375C) | 1.76 | 1.27 [1.27 to 1.34] | 1.13 | 1.25 | 1.03 |
| Ice Lake (8358P), local, shared host | 1.50 [1.35 to 1.76] | 1.21 [1.16 to 1.32] | 1.18 [1.06 to 1.30] | 1.07 [1.07 to 1.20] | 1.10 [1.05 to 1.28] |
| Cascade Lake (8275CL) | 1.26 | 1.64 | 1.75 | 0.98 [0.98 to 1.02] | 1.05 |
| Graviton4 (Neoverse V2) | 1.02 | 1.00 | 1.16 | 1.10 | 1.03 |
| Graviton3 (Neoverse V1) | 1.02 | 0.93 | 1.20 | 1.09 | 1.02 |
| Graviton2 (Neoverse N1) | 0.80 | 0.96 | 1.05 | 1.39 | 1.32 |
| Cortex-X925, local | 1.10 | 1.06 | 1.29 | 1.21 | 1.01 |

### Gaps that remain, tracked in issue #29

Cells whose median is below 0.97, in GiB/s. The last column says whether a kernel already in the build, other than the one selected, beat `crc-fast` in the same run.

| Processor | Checksum | Size | Ratio | `crc-fast` | Crate | Best kernel in the build | Faster kernel in the build |
|---|---|---|---|---|---|---|---|
| Zen 5 | CRC-32C | 256 B | 0.67 | 20.2 | 13.6 | 15.7 `sse-pclmulqdq x4` | no |
| Zen 5 | CRC-32C | 1 KiB | 0.67 | 46.8 | 31.6 | 32.9 `avx512-vpclmulqdq x4` | no |
| Granite Rapids | CRC-32C | 64 KiB | 0.73 [0.72 to 0.97] | 107.4 | 77.9 | 82.6 `avx512-vpclmulqdq x8` | no |
| Sapphire Rapids | CRC-32C | 64 KiB | 0.92 [0.69 to 1.04] | 87.2 | 83.4 | 83.6 `avx512-vpclmulqdq x4 +3x3` | no |
| Graviton4 | CRC-64 | 1 KiB | 0.94 | 22.8 | 21.4 | 24.3 `pmull-eor3 x6` | yes |
| Graviton3 | CRC-64 | 256 B | 0.92 | 10.1 | 9.3 | 13.6 `pmull-eor3 x4` | yes |
| Graviton3 | CRC-64 | 1 KiB | 0.93 | 20.8 | 19.3 | 21.6 `pmull-eor3 x6` | yes |
| Graviton3 | CRC-64 | 64 KiB | 0.96 | 34.3 | 32.8 | 34.2 `pmull-eor3 x6 +3x2` | no |
| Graviton3 | CRC-32C | 256 B | 0.93 | 13.1 | 12.2 | 17.7 `pmull-eor3 x4` | yes |
| Graviton2 | CRC-64 | 256 B | 0.93 | 5.2 | 4.8 | 5.5 `pmull x8` | yes |
| Graviton2 | CRC-32C | 64 B | 0.80 | 4.2 | 3.4 | 5.0 `pmull x12` | yes |
| Graviton2 | CRC-32C | 256 B | 0.96 | 6.8 | 6.5 | 8.3 `pmull x8` | yes |
| Cortex-X925 | CRC-64 | 256 B | 0.91 | 22.1 | 20.0 | 30.7 `pmull-eor3 x4` | yes |
| Cortex-X925 | CRC-64 | 1 KiB | 0.90 | 54.4 | 49.0 | 55.6 `pmull-eor3 x6 +3x2` | yes |

Just above the line, and the case S3 uploads are in: CRC-64 at 8 MiB on Graviton3 and Graviton4 is 0.97 in all three runs.

How the report reads them:

- **Arm, short lengths.** Nine of the ten Arm cells have a narrower kernel in the build that is faster. Choosing the lane count by length would close them without a new kernel.
- **Zen 5, CRC-32C at 256 B and 1 KiB.** No kernel in the build comes near. It needs another method at short lengths on that processor.
- **Granite Rapids and Sapphire Rapids, CRC-32C at 64 KiB.** Noisy: `crc-fast` and the kernels each read two different speeds across runs on one instance. To be measured again on a quiet machine before either number is taken as settled.
- **Graviton3 and Graviton4, CRC-64 on large bodies.** 2 to 4% behind, and steady.

### What the changes after the first run did

| Cell | First run | Second run |
|---|---|---|
| Cascade Lake, CRC-32C, 64 KiB | 0.60 | 0.98 |
| Graviton2, 3 and 4, CRC-64, 1 KiB | 0.85, 0.76, 0.78 | 1.03, 0.93, 0.94 |
| Graviton2, 3 and 4, CRC-32C, 1 KiB | 0.82, 0.88, 0.93 | 1.05, 1.20, 1.16 |
| Zen 4 and Zen 5, CRC-32C, 64 KiB and 8 MiB | 1.00 | 1.15 to 1.24 |
| Cortex-X925, CRC-64, 1 KiB | 0.92 | 0.90 |

### Not covered

- Throughput through the whole client on a large instance.
- Google Cloud Storage: CRC-32C was checked against the reference only.
- Processors older than Cascade Lake and Zen 3, and targets served by the table kernel.

## First run (commit `dffc639`), kept as history

### Correctness

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

### Speed: the crate as selected, relative to `crc-fast` 1.10

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

### What was concluded then

On these figures alone the change's first rule (as fast as `crc-fast` on every benchmark machine at 64 KiB and 8 MiB) was not met: CRC-64/NVME was behind on Graviton3 and Graviton4, and CRC-32C on Sapphire Rapids and Cascade Lake. That led to the changes below and to the second run. The rule itself was replaced by the decision at the top of this file.

### Changes made in answer, measured by the second run

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

The 256-bit and 512-bit kernels were rewritten by (1), (3) and (5). The second run tested them on every machine that has them.

### Caveats of the first run

- One run per cell, three for 1 KiB and 64 KiB. `.large` instances share their memory bandwidth and their processors with other tenants.
- Processors older than Cascade Lake and Zen 3, and targets served by the table kernel, were not run.
