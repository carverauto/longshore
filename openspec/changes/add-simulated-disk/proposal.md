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
- The virtual world's files keep what is durable apart from what has been
  written since. A sync makes a file's writes durable. A directory sync
  makes its renames, unlinks and new files durable
- Power loss, for the whole simulated machine: operations in flight get no
  completion, sockets and file slots are gone, and each write not yet
  synced is kept, dropped, or torn at a sector boundary, by the seed
- Faults drawn from the seed, each with its own rate: short writes, torn
  and lost writes that report an error or stay silent, writes to the wrong
  place, read errors that last until the sector is rewritten, flipped bits
  on read, failed syncs, and refused reserves and punches
- Faults placed by the test: the file operation with a given number fails
  before or after taking effect, or in one of the ways a write fails
- A sector size that the test sets, 512 or 4,096
- The rules of UltraGraphDB's `SimStorage` are the starting point. The
  code is written anew for operations and completions, and the origin is
  recorded

## Impact
- Affected specs: `simulated-disk` (new)
- Affected code: `longshore_common` (`sim`, the virtual world), `NOTICE`
- Depends on: `add-file-operations`
- First principles: 5, a caller's storage code runs on the kernel and on
  this disk unchanged. Within a simulated machine there is one thread, one
  seed and one virtual clock for files and sockets together. All of the
  disk's memory is taken when the test sets it up
- Outside this repository: UltraGraphDB replaces `SimStorage` with this
  model in a change of its own
