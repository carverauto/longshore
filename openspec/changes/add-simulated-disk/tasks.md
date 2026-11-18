## 1. State
- [ ] 1.1 The setup call gains the write log's capacity, the read-error capacity and the sector size
- [ ] 1.2 Durable bytes with size and holes; the log of unsynced writes laid over them on read; sync folds it in; the oldest write made durable when the log is full
- [ ] 1.3 Directories: durable names and unsynced changes; a directory sync; an open file outliving its name
- [ ] 1.4 Existing file tests pass unchanged; trace hashes of existing seeds unchanged

## 2. Power loss
- [ ] 2.1 The call: drop every operation in flight, free sockets and file slots, clear timers and the waker flag
- [ ] 2.2 Settle each unsynced write and directory change in order by the seed; cuts on sector boundaries inside the write
- [ ] 2.3 Tests: synced writes survive; a single write takes one of the three outcomes; overlapping writes; the same seed gives the same bytes; callers rebuilt on both ends of a connection

## 3. Faults
- [ ] 3.1 `DiskFaults` set through its own call, one rate per fault, drawn with `chance`
- [ ] 3.2 The five write faults; a short write ends a chain
- [ ] 3.3 Writes to the wrong place
- [ ] 3.4 Read errors per sector, cleared by a covering write or punch; flipped bits on read; the test call that flips a bit on the medium
- [ ] 3.5 Failed syncs whose writes never become durable; refused reserves and punches with their errors
- [ ] 3.6 Placed faults numbered at submission: fail, fail after, and the write faults
- [ ] 3.7 Counters per fault kind
- [ ] 3.8 Each fault's comment names the real failure it stands for

## 4. From UltraGraphDB
- [ ] 4.1 The rules of `SimStorage` carried over from the files the design lists, with the table of differences from the design in the module's comments
- [ ] 4.2 The tests of `tests/types/sim_storage/sim_storage_tests.rs` written again against `Io`; those with no counterpart listed in the pull request
- [ ] 4.3 A write-ahead log test in this repository: append, sync, power loss, recover, under every fault, across a range of seeds
- [ ] 4.4 This project's header on the new files; the origin recorded in their comments and in `NOTICE`

## 5. Project
- [ ] 5.1 README: the simulated disk, its faults and its setup
- [ ] 5.2 `openspec/project.md`: disk faults in the testing strategy
