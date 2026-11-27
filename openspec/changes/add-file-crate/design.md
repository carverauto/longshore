## Context
`longshore_common` offers file I/O as operations of the `Io` trait
(`Op::FileOpen` to `Op::FileMkdir`, `Op::FileWriteSpans`), fixed file slots,
direct I/O, chains (`Io::submit_chain`) and a simulated disk with faults and
power loss (specs `file-operations`, `simulated-disk`). What it does not
offer is a layer that owns the plumbing around them, as `longshore_tcp`'s
`Net` does for sockets. This change adds that layer, for files only.

## Goals / Non-Goals
- Goals: one API for file I/O over every backend; handles that cannot reach
  a reused slot; whole transfers; ordered sequences; direct I/O checked the
  same way everywhere; errors the same everywhere; nothing allocated after
  build; tested on all three backends and under every fault the simulated
  disk has.
- Non-Goals: anything a user of files builds on top (logs, segments, pages,
  records, caches, recovery of a format); checksums of bodies; a page cache;
  an async runtime; truncate and directory listing.

## Decisions
- **The crate sits where `longshore_tcp` sits.** A layer of state machines
  over the `Io` trait that does no I/O itself (first principle 5): the
  same code runs on `UringIo`, `PosixIo` and `SimIo`. The caller owns the
  backend and the loop.
- **The same driving API as `Net`.** `FileConfig::plan(limits, sizes)`
  returns the `Limits` with the set's file slots, operations and tokens
  added, and the set's share of them; `Files::new(config, &limits)` makes
  every table once; `on_completion(io, c) -> bool` takes the set's
  completions; `poll(io, now)` submits what waits; `next_event()` reports
  outcomes. Sets of files, connections and the S3 client then share one
  ring and one thread.
- **Tickets, not tokens.** Each operation returns a `Ticket` (slot and
  generation) or a refusal at once (`Full`, `Stale`, `Unaligned`,
  `TooLarge`, `Closed`); its outcome is an event naming the ticket. The set
  keeps the backend's tokens to itself.
- **Whole transfers.** A read or write is resumed after a short completion
  until it is done, the file ends (a read) or an error ends it; the event
  says how much was transferred. A caller that wants the raw behaviour has
  `longshore_common`'s operations.
- **Ordered sequences on the backend's chains.** A sequence (for example a
  write then a data sync) is submitted as one chain; a failure cancels the
  rest, and every operation of it still reports. Bounded by the chain room
  the plan gives.
- **Memory through `Extents`.** Buffers are the block-aligned class of the
  pool, taken and held through `longshore_common`'s `Extents`, so a buffer
  written to a file and sent on a socket is the same buffer, held by each
  until done. The set holds a buffer for as long as an operation reads or
  writes it.
- **Alignment checked before submission.** For a file opened for direct
  I/O, offsets, lengths and buffers are checked against the block size
  before anything is submitted; every backend then sees only what it would
  take.
- **Errors made one.** Backend results map to one `FileError`. A failed
  sync is stated precisely: the bytes written since the last good sync are
  of unknown state, the file stays open, and a later sync does not make
  them good (the semantics the simulated disk already models).
- **Bounded everything.** Files, operations in flight, operations waiting
  for room, sequence lengths: all sized by the plan, refused beyond it.

## The contract

### C1 Layer
`longshore_file` is a set of state machines over `longshore_common::io::Io`.
It submits operations and takes completions; it never blocks, sleeps,
spawns or reads a clock of its own. `Files::poll(io, now)` is its only
place of submission besides the call that asked for an operation (which may
submit at once if there is room).

### C2 Plan and bounds
`FileSizes` names, and `FileConfig::plan(limits, sizes)` adds to `Limits`:

| Size | What it bounds |
|---|---|
| `files` | open files at once (file slots) |
| `ops` | operations in flight or waiting, in the whole set |
| `ops_per_file` | operations of one file at once; 1 ≤ it ≤ `ops − files + 1` |
| `sequence_max` | operations in one ordered sequence |
| `paths` | operations naming a path at once (open, status, rename, unlink, mkdir), each holding a small buffer for its path |
| `spans_max` | spans of one gathered write |

**No file can take every slot.** Of the `ops` operation slots, `files` are
reserved, one for each file slot, and the rest are shared. An open file
uses its reserved slot first, then shared slots, up to `ops_per_file` at
once. So every open file can always start an operation, whatever the
others do, and one busy file holds at most `ops_per_file − 1` shared
slots. The plan refuses sizes that break `1 ≤ ops_per_file ≤ ops −
files + 1` (`ConfigError::Ops`). The backend's own room (`Limits::ops_max`,
chains, gathered writes) is added by the plan for these counts.

### C3 A file handle
`File { slot, generation }`. States of a slot:

| State | Entered by | Accepts | Left by |
|---|---|---|---|
| Free | build; a close's completion | `open` | `open` accepted → Opening |
| Opening | `open` accepted | nothing but the open's completion | success → Open; failure → Free, the open's event carries the error |
| Open | the open's success | every operation | `close` → Closing; a failed sync → Poisoned |
| Poisoned | a sync of the file failed | `close` only; others refused with `FileError::Poisoned` | `close` → Closing |
| Closing | `close` accepted | nothing | the close's completion, after every operation of the file has reported → Free, generation + 1 |

A handle whose generation is not the slot's is stale: `FileError::Stale`,
and nothing is submitted.

### C4 Tickets and events
Each accepted operation gets `Ticket { index, generation }` of its
operation slot. Its outcome is one `Event::Done { ticket, file, result }`
with `result: Result<u64, FileError>` (bytes transferred, or 0 for
operations without bytes). The event queue holds `ops` events, so it never
overflows: an operation slot is freed only when its event is taken
(`next_event`). Every ticket reports exactly once.

### C5 Operations
| Operation | Holds while in flight | Result |
|---|---|---|
| `open(path, OpenFlags)` | a path buffer | the `File`, via its event |
| `close(file)` | – | 0 |
| `read_at(file, extent, range, file_off)` | the extent | bytes read; fewer only at end of file |
| `write_at(file, extent, range, file_off)` | the extent | bytes written: all, or an error |
| `write_spans(file, spans, file_off)` | each span's extent, the set's list buffer | as `write_at` |
| `sync(file, data_only)` | – | 0 |
| `reserve(file, range, keep_size)`, `punch(file, range)` | – | 0 |
| `status(path)` | a path buffer | the size, via a status the caller reads from the event |
| `rename(from, to)`, `unlink(path)`, `mkdir(path)` | path buffers | 0 |

Paths are copied into the set's path buffers (a path is not payload).

### C6 Whole transfers
A read or write completed short is resubmitted for the rest at once,
keeping its ticket. A read that returns 0 bytes ends at end of file. An
error ends the transfer; the event carries the error and, in
`FileError::Partial { done, error }`, the bytes done before it.

### C7 Ordered sequences
`sequence(&[step])` takes up to `sequence_max` steps of one file (any but
`open` and `close`) and gives a ticket for each. They go to the backend as
one chain. When the backend ends a chain early because a transfer was
short, the set resumes that transfer and submits the rest of the sequence
as a new chain, so a later step still starts only after the earlier one is
whole. When a step fails, the rest report `FileError::Cancelled`. A
sequence counts its steps against the file's and the set's slots.

### C8 Memory
Data buffers are `Extent`s of the caller's `Extents` (the pool's
block-aligned class). The set takes a hold on each extent an operation
names when it accepts the operation and releases it when the operation
reports, so a buffer is never given out again while the kernel reads or
writes it. The set's own buffers (paths, span lists) are planned in C2.
No payload is copied.

### C9 Direct I/O
On a file opened with `OpenFlags::with_direct`, before anything is
submitted: each buffer range starts at a block boundary of its extent, and
each length and file offset is whole blocks (`Limits::block_size`);
otherwise `FileError::Unaligned`. A resumed transfer stays aligned because
backends cut direct transfers at block boundaries; if one does not, the
rest is refused as `Unaligned` in the event.

### C10 Errors
| Backend result | `FileError` |
|---|---|
| `-ENOENT` | `NotFound` |
| `-EEXIST` | `Exists` |
| `-EACCES`, `-EPERM` | `Denied` |
| `-ENOSPC`, `-EDQUOT` | `NoSpace` |
| `-EIO` on a sync | `SyncFailed`, and the file becomes Poisoned |
| `-EIO` otherwise | `Io` |
| `-EINVAL` | `Invalid` |
| `-ECANCELED` in a chain | `Cancelled` (or resumed, C7) |
| anything else | `Os(errno)` |

**A failed sync poisons the file**: the bytes written since its last good
sync are of unknown state, and no later sync makes them durable (as the
kernel may already have dropped them, and as the simulated disk models).
The file then accepts only `close`; the caller recovers by its own means.

### C11 Refusals at the call
`Full` (no operation slot for this file or the set, or no path buffer),
`Stale`, `Poisoned`, `Closing`, `Unaligned`, `TooLarge` (more spans or
steps than planned, a range past its extent), `NotOpen` (a read or write on
a file opened without read or write). A refusal submits nothing and takes
no slot.

### C12 What the crate does not detect
Silent faults (a lost, torn or misdirected write reported as done, a
flipped bit) are invisible without checksums of the bytes, which the crate
leaves to its users. After a power loss the set is dropped with the
backend; its user opens the files again and reads what survived.

### Invariants, checked after every step of the model test
- F1 No operation is submitted on a stale handle or a slot not Open
  (or Closing, for the close itself).
- F2 Every accepted ticket reports exactly once; no event names an
  unknown ticket.
- F3 Every extent named by an operation in flight is held by the set; no
  hold of the set outlives its operation's event.
- F4 A file's operations in flight never exceed `ops_per_file`; the set's
  never exceed `ops`; every open file has an operation slot it can use.
- F5 The steps of a sequence start in order, each after the one before is
  whole; after a failed step the rest are cancelled.
- F6 A file whose sync failed accepts nothing but `close`.
- F7 A read returns the bytes of the last whole writes to that range, when
  no silent fault is on.
- F8 After a power loss, every byte an event reported synced (a sync that
  succeeded after the write completed) is there when the files are read
  again, when no silent fault is on.
- F9 Nothing is allocated after `Files::new`.
- F10 At rest (no operation in flight) every slot's state agrees with the
  events taken: no slot Opening or Closing, no operation slot used.

## Risks / Trade-offs
- An API that does too much becomes a storage engine → the non-goals above
  are part of the spec, and the review of each group checks against them.
- Resuming short transfers hides partial progress → the event says how much
  was transferred, and a failure part-way through says so.
- Chains differ between backends (the ring links operations, the portable
  backend and the simulator order them in the engine) → the contract suite
  runs every sequence on all three.

## Migration Plan
Nothing to migrate: the crate is new, and `longshore_common`'s operations
stay.

## Open Questions
- None open: the per-file bound is C2 (a reserved operation slot for every
  file and a cap per file), as the user asked.
