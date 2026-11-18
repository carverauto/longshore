## 1. Two states
- [ ] 1.1 Written and durable state for files, with size and holes; sync copies one to the other
- [ ] 1.2 The same for directory names: rename, unlink and new files
- [ ] 1.3 Existing file tests pass unchanged; trace hashes of existing seeds unchanged

## 2. Power loss
- [ ] 2.1 The call: drop operations in flight, settle each unsynced write by the seed, cut at 512-byte boundaries
- [ ] 2.2 Tests: synced writes survive; unsynced writes take one of the three outcomes; the same seed gives the same bytes

## 3. Faults
- [ ] 3.1 `DiskFaults` with one rate per fault; no draw when a rate is zero
- [ ] 3.2 Short, torn and lost writes, reported and silent
- [ ] 3.3 Writes to the wrong place
- [ ] 3.4 Read errors that last until rewritten; flipped bits on read
- [ ] 3.5 Failed syncs that discard; refused reserves and punches
- [ ] 3.6 Placed faults by operation number
- [ ] 3.7 Each fault's comment names the real failure it stands for

## 4. From UltraGraphDB
- [ ] 4.1 Carry over the behaviour of `SimStorage` and its fault modes; list any that differ and why
- [ ] 4.2 Carry over its tests as tests of this model against `Io`
- [ ] 4.3 A write-ahead log test in this repository: append, sync, power loss, recover, under every fault, across a range of seeds
- [ ] 4.4 Donated code carries this project's Apache 2.0 header (`scripts/license-headers.py`)

## 5. Project
- [ ] 5.1 README: the simulated disk and its faults
- [ ] 5.2 `openspec/project.md`: disk faults in the testing strategy
