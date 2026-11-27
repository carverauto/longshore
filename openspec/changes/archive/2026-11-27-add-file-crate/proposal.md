# Change: A file crate, longshore_file: one API for file I/O

## Why
longshore does file I/O today through `longshore_common`'s raw operations:
a caller picks file slots, buffers and tokens itself, submits `Op::File*`,
routes every completion back by token, resumes short reads and writes,
checks direct-I/O alignment, orders a write before its sync with a chain,
and keeps track of which slot holds which file. Every program that keeps a
log or a store on disk rewrites that plumbing, and each copy can get it
wrong in its own way. A file layer with longshore's rules does it once,
tested once, the way `longshore_tcp` does it for sockets.

The crate is file I/O only, whatever it is used for. It knows nothing of
logs, pages or records: a write-ahead log or a storage engine is built on
it by its user (UltraGraphDB, in its own repository).

## What Changes
- New crate `longshore_file`, re-exported by `longshore` as `file`:
  - `Files`: a fixed set of files and operations, planned from `Limits` at
    build (`FileConfig::plan`) and driven from the caller's loop. It borrows
    the backend on each call and never waits: the caller routes completions
    to it (`on_completion`), calls `poll`, and reads `next_event`, so
    sockets (`longshore_tcp`), the S3 client and files share one ring and
    one thread.
  - Handles with a generation (`File`): a stale handle is refused, never
    routed to a reused slot. Each operation returns a `Ticket` or a typed
    refusal at once, and completes as an event naming its ticket.
  - Operations: open (read, write, create, exclusive, direct I/O, durable
    writes), close, read at an offset, write at an offset, gathered write
    of several spans, sync (data or data and metadata), reserve and punch
    space, rename, unlink, make a directory, and status.
  - Ordered sequences: operations that must happen in order (a write and
    then its sync, a sync and then a rename) go to the backend as one
    chain, and a failure ends the rest.
  - Whole transfers: short reads and writes are resumed by the set until
    the transfer is complete, the file ends or an error ends it.
  - Memory: buffers are the pool's block-aligned class, lent through
    `Extents`; the set checks direct-I/O alignment before it submits, so a
    misaligned transfer is the same typed refusal on every backend. No
    payload is copied.
  - Errors: one `FileError`, the same on every backend, including what a
    failed sync means for the bytes before it.
- `longshore_common`'s file operations stay as they are: the crate is built
  on them, and a program that wants the raw operations keeps them.
- Tests: a contract suite run on `SimIo`, `PosixIo` and `UringIo`; a model
  test on the simulated disk with its faults, failed syncs, torn writes and
  power loss, checking the contract's invariants after every step; the
  allocation guard; a real-kernel self-check; property tests of the
  planning and of resumed transfers.
- Measured on real disks (this Mac, and the DGX Spark's NVMe), results in
  `bench/results/`; `docs/file.md`.
- **Not in this change:** logs, segments, pages, records, caches, or any
  checksum of bodies; truncate and directory listing (left out of the ring
  on Linux 6.1, as `openspec/project.md` says).

## Impact
- Affected specs: `file-io` (ADDED); `file-operations` and
  `simulated-disk` unchanged.
- Affected code: new `crates/longshore_file`; `crates/longshore` (the
  `file` re-export); `crates/longshore-bench` (a self-check and a bench);
  `docs/file.md`.
