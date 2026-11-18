## Context
The ring backend registers one `iovec` per pool buffer and uses the buffer's
number as the registered index, cut to 16 bits (`io/uring/backend.rs:129`).
Its registered file table holds sockets only. `ReadFile` is three steps
driven by three completions: open, read, close. The pool is one allocation
with no alignment. Results are a 32-bit integer.

## Goals / Non-Goals
- Goals: every file operation a log and a block store need, on Linux 6.1;
  write-then-sync in one submission; no copy between buffer and disk; the
  same operations and the same errors on every backend
- Non-Goals: truncate; buffered I/O tuning; reads or writes larger than a
  buffer; directory listing; disk faults in the simulator, which
  `add-simulated-disk` adds; a thread pool

## Decisions
- Files live in fixed slots named by `FileId`, as sockets live in slots
  named by `Sock`. The caller picks the slot. `Limits` says how many there
  are. In the ring they are registered files, opened straight into the
  table, so the process holds no descriptor for them.
- A path is bytes in a pool buffer, named by buffer, offset and length. The
  operation owns the buffer until it completes, as any operation does. No
  path is stored and nothing allocates.
- `register_file` and `ReadFile` stay as they are for the credentials file.
- Reads and writes name a file, a buffer range and a 64-bit offset, and
  complete with the bytes moved, which may be fewer than asked.
- Pools are registered as ranges of at most 1 GiB. An operation names the
  range a buffer lies in and the buffer's address. The kernel's cap on the
  number of registered buffers is then never near, and the 16-bit cut goes.
  Alternative considered: keep one registration per buffer. Rejected
  because a storage engine's pool would reach the cap.
- An aligned pool is a third class beside the large and small buffers. Its
  block size is a power of two, at least 4,096, and each buffer's address
  and length are multiples of it. The memory is one allocation, started at
  an aligned address inside it.
- A file opened for direct I/O takes only buffers from the aligned pool,
  and offsets and lengths that are multiples of the block size. The engine
  checks this before the operation reaches a world or the ring, and
  completes it with the invalid-argument error. A mistake is therefore
  caught in the simulator on any host.
- The locked-memory limit is read when the backend is built. If the pools
  are larger, building fails and reports the pool size and the limit.
- A chain is submitted with one call that takes all its operations or none
  (`Busy`). They run in order. When one fails, those after it complete as
  cancelled. Each still has exactly one completion. In the ring a chain is
  linked submissions; the engine runs a chain's operations one at a time.
  Alternative considered: a flag on each operation. Rejected because a
  chain half accepted cannot be undone.
- File status is written into a pool buffer and read with a decoder, as a
  connect's segment sizes are read with `Segment::decode`.
- There is no truncate. Linux has it on the ring only from 6.9. The
  alternatives were a higher kernel floor or a blocking call on the I/O
  thread, and each breaks a rule. Reserve, punch and unlink cover a
  preallocated log and a block store.
- In the POSIX world the calls are `pread`, `pwrite`, `fsync` and their
  like, made on the thread that calls `tick`. On macOS direct I/O is the
  no-cache setting and a full sync is `F_FULLFSYNC`. That backend serves
  development and tests, so a blocked thread is accepted there.
  Alternative considered: a helper thread. Rejected: it brings a second
  thread and shared buffers for a backend that makes no performance claim.
- In the virtual world a file is bytes in memory with a size and holes.
  Operations complete after a seeded delay. Nothing is lost or torn in this
  change.

## Risks / Trade-offs
- A range of 1 GiB may fail to register where memory is fragmented or the
  limit is low. → Building fails with the reason; nothing fails later.
- macOS and Linux differ in what "synced" means. → The contract is stated
  in Linux terms and the POSIX world uses the strongest macOS call.
- Chains change the ring backend's rule that a slot is one submission. →
  The shared tests cover a chain cancelled at each position.

## Migration Plan
1. Limits, the aligned pool, registration by range, the locked-memory
   check. No new operation; the S3 path is measured before and after.
2. The file table and the operations, ring first, then both worlds.
3. Chains.
Each step is its own commit.

## Open Questions
None.
