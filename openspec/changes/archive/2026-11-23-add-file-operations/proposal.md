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
- A table of file slots of fixed size, named by a new `FileSlot`. A file is
  opened into a slot, with its path taken from a pool buffer, and is a
  registered file in the ring. The backend tracks which slots are in use
- Reads and writes at an offset, between a file and a registered buffer
- Sync, of data only or of data and metadata
- Reserving space, keeping or extending the file's size, and punching holes
- File status by path, rename, unlink and make-directory
- Chains: file operations submitted together run in order. A chain ends at
  the first member that fails or moves fewer bytes than asked, and the rest
  are cancelled. A write and its sync go out in one submission
- A pool of buffers aligned to a block size, for files opened for direct
  I/O. Pools are registered with the ring as a few large ranges
- One check, called by the ring backend and by the engine alike, refuses a
  misused slot, a bad path or a misaligned direct operation before the
  kernel or a world sees it
- The same operations in the POSIX world, made on the calling thread, and
  in the virtual world on files held in memory
- The ring is set up to submit everything it is given, and checks that it
  did. Without that, one operation the kernel refuses to prepare strands
  every operation queued behind it, which a caller's bad path could now
  cause
- No truncate operation and no directory listing. A file can be emptied
  when it is opened
- **BREAKING**: `Limits` gains fields for the file table and the aligned
  pool; `Io` gains a method that submits a chain

## Impact
- Affected specs: `file-operations` (new)
- Affected code: `longshore_common` (`io`, the ring backend, the engine and
  both worlds), `longshore_s3` (the test wrapper that forwards `Io`),
  `longshore-bench` (a real-kernel check for files),
  `scripts/test-linux.sh`, `.github/workflows/ci.yml`
- Depends on: `add-portable-io-backend`
- First principles: 1, every file operation completes through the one
  queue; in the POSIX world a file call blocks the calling thread, which
  that backend's purpose allows. 2, the table and the pools are sized when
  the backend is built. 3, a buffer goes to the disk and comes from it
  without a copy. No dependency is added
- Kernel: every operation here is in Linux 6.1. On 6.1 the kernel runs
  sync, reserve, punch, rename, unlink, make-directory and an open that
  creates on its own worker threads; their results still arrive through
  the one queue
