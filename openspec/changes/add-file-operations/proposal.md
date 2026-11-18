## Why
The only file operation longshore has is `ReadFile`, which reads a whole
file from its start. A storage engine needs reads and writes at an offset
from aligned registered buffers, a sync that is ordered after the write it
covers, and space that can be reserved and given back. UltraGraphDB does
this through compio today: a log write and its sync are two operations
awaited one after the other, each buffer is allocated for its one use, and
allocation goes to a thread pool. It is moving that path to longshore
(https://github.com/carverauto/longshore/issues/40).

## What Changes
- A table of file slots of fixed size. A file is opened into a slot, with
  its path taken from a pool buffer, and is a registered file in the ring
- Reads and writes at an offset, between a file and a registered buffer
- Sync, of data only or of data and metadata
- Reserving space and punching holes
- File status, rename, unlink and make-directory
- Chains: operations submitted together run in order, and the rest are
  cancelled when one fails. A write and its sync go out in one submission
- A pool of buffers aligned to a block size, for files opened for direct
  I/O. Pools are registered with the ring as a few large ranges
- Building fails, with both numbers, when the pools exceed the
  locked-memory limit
- The same operations in the POSIX world, made on the calling thread, and
  in the virtual world on files held in memory
- No truncate operation. A file gets its size when space is reserved and
  gives it back through punched holes
- **BREAKING**: `Limits` gains fields for the file table and the aligned
  pool

## Impact
- Affected specs: `file-operations` (new)
- Affected code: `longshore_common` (`io`, the ring backend, the engine and
  both worlds), `longshore-bench` (a real-kernel check for files),
  `scripts/test-linux.sh`
- Depends on: `add-portable-io-backend`
- First principles: 1, every file operation completes through the one
  queue; in the POSIX world a file call blocks the calling thread, which
  that backend's purpose allows. 2, the table and the pools are sized when
  the backend is built. 3, a buffer goes to the disk and comes from it
  without a copy. No dependency is added
- Kernel: every operation here is in Linux 6.1
