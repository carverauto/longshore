Each group ends with a review of its own diff; its findings are fixed before
the next group starts. The no-mistakes gate runs once, at the end of the
change.

## 0. The contract
- [ ] 0.1 `design.md`: the state tables of a file handle and of an operation, the table of every backend result against what the set does, the invariants, and an answer for every bound that can be reached
- [ ] 0.2 Review of the contract; corrections made there first

## 1. The crate and its plan
- [ ] 1.1 `crates/longshore_file` (Apache headers, workspace lints), re-exported by `longshore` as `file`
- [ ] 1.2 `FileSizes`, `FileConfig::plan` adding file slots, operations, tokens and chain room to `Limits`, with typed refusals for what does not fit
- [ ] 1.3 `Files::new`, handles with generations, tickets, `on_completion`, `poll`, `next_event`
- [ ] 1.4 Review of group 1

## 2. Operations
- [ ] 2.1 Open and close, with every flag of `OpenFlags`; status, rename, unlink, mkdir
- [ ] 2.2 Read and write at an offset, resumed to the whole transfer; gathered writes
- [ ] 2.3 Sync (data, and data and metadata), reserve, punch
- [ ] 2.4 Ordered sequences on the backend's chains
- [ ] 2.5 Buffers through `Extents`, held while an operation uses them; direct-I/O alignment checked before submission
- [ ] 2.6 One `FileError`, and the failed-sync semantics of the contract
- [ ] 2.7 Review of group 2

## 3. Tests
- [ ] 3.1 The contract suite, generic over `IO: Io`, run on `SimIo`, `PosixIo` (macOS and Linux) and `UringIo` (`file_uring`)
- [ ] 3.2 The model test on the simulated disk: seeded operations, disk faults by rate and placed, failed syncs, torn writes and power loss, the invariants after every step, replay by seed
- [ ] 3.3 Under the allocation guard: nothing allocated after build
- [ ] 3.4 Property tests: the plan, resumed transfers, alignment checks
- [ ] 3.5 Every row of the contract's tables reached by a test; deliberate faults in the crate caught (mutation spot-checks)
- [ ] 3.6 Review of group 3

## 4. Measurement and documentation
- [ ] 4.1 A bench through the crate against the raw operations, on this Mac and on a DGX Spark's NVMe: buffered and direct, several operations in flight, syncs; results in `bench/results/`
- [ ] 4.2 `docs/file.md`: the API, sizing, direct I/O, ordering and durability, what a failed sync means, and what the crate does not do
- [ ] 4.3 Review of group 4
