## Context
After `add-file-operations` the virtual world holds each file as bytes in
memory, and every operation succeeds. Network faults are rates in parts per
million in `Faults`, drawn from the one generator, and a run is identified
by its trace hash.

UltraGraphDB's `SimStorage` keeps a buffered and a stable view of each file
and directory, and offers power loss, a fault at a numbered operation,
random faults, corruption and a refused preallocation. Its fault modes are
before, after, torn, silently torn and short.

## Goals / Non-Goals
- Goals: every way a disk fails that a log or block store must survive;
  the same seed gives the same disk; files and sockets under one clock;
  nothing a real kernel could not do
- Non-Goals: modelling a file system's journal or a device's wear;
  performance; faults on the portable backend's real files

## Decisions
- A file is two byte arrays, written and durable, with a size and holes
  for each. A write changes the written state. A sync copies the file's
  written state to the durable one. Memory for both is taken when the test
  sets the disk up.
- A directory has the same two states for its names. A rename, an unlink
  or a new file is durable only after the directory is synced.
- Power loss is a call on the simulator, not an operation. Every operation
  in flight is dropped without a completion, as a process that lost power
  sees none. Each write since the last sync is then kept whole, dropped, or
  kept up to a 512-byte boundary, by the seed. The written state becomes
  the result, and the test opens the files again.
- Disk faults are a `DiskFaults` value beside `Faults`, with one rate per
  fault. With every rate at zero the generator is not consulted, so the
  traces of existing tests do not change.
- A torn or lost write that the kernel would report completes with an
  error. A silent one completes with success. Both exist, because a store
  must detect the second by its own checksums.
- A write to the wrong place lands at another block-aligned offset of the
  same file and completes with success.
- A read error marks a range. Reads of it fail with the I/O error until a
  write covers it.
- A failed sync completes with the I/O error and its unsynced writes are
  dropped from the written state, as Linux drops the pages. A later sync
  that succeeds does not bring them back.
  Alternative considered: keep the writes for a retry. Rejected because it
  is what stores once assumed and what Linux does not do.
- A placed fault names an operation by its number among file operations
  and the way it fails. It overrides the rates for that operation.
- The model's behaviour is taken from `SimStorage` and its tests are
  carried over as tests of this model, written against `Io`.

## Risks / Trade-offs
- The model may allow a state no real disk produces, and a store would
  then be hardened against nothing. → Each fault is tied to a documented
  failure of real devices or of Linux, named in the code.
- Two copies of every file double the simulator's memory. → Files in tests
  are small, and the size is set by the test.

## Migration Plan
1. Two states and sync, with no faults: existing file tests unchanged.
2. Power loss.
3. Faults by rate, then placed faults.
UltraGraphDB moves to this model after step 3, in its own repository.

## Open Questions
None.
