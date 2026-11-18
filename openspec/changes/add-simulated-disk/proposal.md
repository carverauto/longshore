## Why
The simulator injects faults on the network and none on the disk. With file
operations added, a log or a block store written against `Io` would be
tested on a disk that never loses power and never tears a write.
UltraGraphDB has such a disk already, `SimStorage`, with power loss, torn
writes and targeted faults, behind its own storage trait. It is moving its
storage I/O to longshore
(https://github.com/carverauto/longshore/issues/40), and two simulated
disks could not share a seed or a clock with the simulated network.

## What Changes
- The virtual world's files keep two states: what has been written and what
  is durable. A sync makes a file's writes durable. A directory sync makes
  its renames, unlinks and new files durable
- Power loss: the durable state survives, and writes not yet synced are
  each kept, dropped, or torn at a 512-byte boundary, by the seed
- Faults drawn from the seed, each with its own rate: a short write, a
  torn write, a lost write, a write to the wrong place, a read error that
  stays until the range is rewritten, a flipped bit on read, a failed sync,
  and a refused reserve or punch
- Faults placed by the test: the operation with a given number fails in a
  given way
- UltraGraphDB's `SimStorage` and its tests are the starting point; the
  model moves here and is extended

## Impact
- Affected specs: `simulated-disk` (new)
- Affected code: `longshore_common` (`sim`, the virtual world)
- Depends on: `add-file-operations`
- First principles: 5, a caller's storage code runs on the kernel and on
  this disk unchanged. The simulator stays one thread, one seed and one
  virtual clock for files and sockets together
- Outside this repository: UltraGraphDB replaces `SimStorage` with this
  model in a change of its own
