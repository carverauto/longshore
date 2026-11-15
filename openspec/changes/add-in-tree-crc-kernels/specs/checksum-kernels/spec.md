## ADDED Requirements

### Requirement: In-tree checksum kernels
The checksum crate SHALL compute CRC-64/NVME and CRC-32C with its own code,
without a dependency outside the repository in what ships.

#### Scenario: Published check values
- **WHEN** either checksum is computed over the bytes `123456789`
- **THEN** the result is the catalogue's check value for that algorithm

#### Scenario: Agreement with a reference
- **WHEN** any kernel the processor supports computes a checksum over any input, at any length and alignment
- **THEN** the result equals a bit-at-a-time computation over the same input

#### Scenario: Data in pieces
- **WHEN** an input is fed to an incremental checksum in pieces split at any points
- **THEN** the result equals the one-shot checksum of the whole input

#### Scenario: No allocation
- **WHEN** a checksum is computed with heap allocation sealed
- **THEN** it completes

### Requirement: Kernel selection at run time
The checksum crate SHALL choose, when the program runs, the fastest kernel
the processor supports, and SHALL run on a processor that supports none of
the accelerated kernels.

#### Scenario: AVX-512 with wide carry-less multiply
- **WHEN** an x86-64 processor reports AVX-512 and `VPCLMULQDQ`
- **THEN** a kernel using `VPCLMULQDQ` is used

#### Scenario: Wide carry-less multiply without AVX-512
- **WHEN** an x86-64 processor reports AVX2 and `VPCLMULQDQ` but not AVX-512
- **THEN** the 256-bit kernel is used

#### Scenario: Neither wide form
- **WHEN** an x86-64 processor reports `PCLMULQDQ` and SSE4.2 only
- **THEN** the 128-bit kernel is used

#### Scenario: AArch64
- **WHEN** an AArch64 processor reports `PMULL`
- **THEN** the `PMULL` kernel is used

#### Scenario: No acceleration
- **WHEN** the processor reports none of these, or the target is another architecture
- **THEN** the table kernel is used and the result is the same

### Requirement: Throughput not below the replaced library
An algorithm SHALL move to its in-tree kernel only when a recorded
benchmark shows it at least as fast as `crc-fast` on every benchmark
machine at 64 KiB and 8 MiB.

#### Scenario: Recorded comparison
- **WHEN** an algorithm is switched to the in-tree kernel
- **THEN** `bench/results/` holds the comparison for each benchmark machine

#### Scenario: A kernel that is slower
- **WHEN** the in-tree kernel for an algorithm is slower than `crc-fast` on a benchmark machine
- **THEN** that algorithm stays on `crc-fast` and the result is recorded
