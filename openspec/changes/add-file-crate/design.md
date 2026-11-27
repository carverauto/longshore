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

## Contract to write first (group 0)
State tables for a file handle (free, opening, open, closing; stale after
close) and for an operation (waiting, submitted, resumed, done), the table
of every backend result against what the set does with it, and the
invariants checked after every step of the model test: no operation
reaches a stale handle; a buffer is held while an operation uses it; every
ticket reports exactly once; ordered sequences complete in order or end at
the first failure; nothing is allocated after build; and after a power
loss, what the simulated disk kept agrees with what the events said was
synced.

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
- Whether a file's operations should be limited per file as well as per
  set, so that one busy file cannot take every slot: settled in group 0
  from the contract.
